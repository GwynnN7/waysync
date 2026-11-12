mod capture;
mod config;
mod deploy;
mod engine;
mod hooks;
mod manifest;
mod menu;
mod packages;
mod profile;
mod repo;
mod state;
mod utils;
mod validation;

use std::fs::{File, OpenOptions};
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use config::AppConfig;
use manifest::Manifest;
use menu::MainMenuAction;
use profile::{Device, Profile};
use state::State;

#[derive(Parser)]
#[command(
    name = "waysync",
    about = "Declarative dotfile & package manager for Arch-based systems",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Clone and validate a dotfiles repository (does not deploy anything)
    Init { url: String },
    /// Pull and validate the dotfiles repository only (no deployment or packages)
    Update,
    /// Deploy files for installed, enabled components; no package operations
    Deploy {
        /// Skip post_deploy hooks too: copy files only
        #[arg(long)]
        no_hooks: bool,
        /// Component names; omit for all installed components in the active profile
        components: Vec<String>,
    },
    /// Copy deployed edits from ask=true components into an authoring Git checkout
    Capture {
        /// Authoring checkout; remembered for later captures
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Show changes without copying or saving the checkout preference
        #[arg(long)]
        dry_run: bool,
        /// Copy nonconflicting changes without interactive prompts
        #[arg(long)]
        yes: bool,
        /// With --yes, also overwrite checkout files changed since deployment
        #[arg(long, requires = "yes")]
        force: bool,
    },
    /// Force package reinstall/rebuild only; no files, hooks, or tracking changes
    Reinstall { components: Vec<String> },
    /// Install selected components (packages, files, hooks, and tracking)
    Install {
        #[arg(required = true)]
        components: Vec<String>,
    },
    /// Uninstall selected components (unshared packages, managed files, and tracking)
    Uninstall {
        #[arg(required = true)]
        components: Vec<String>,
    },
    /// Reconcile enabled components and deploy their files, without pulling
    Sync,
    /// Show active profile and component status
    Status,
    /// Select an environment; use 'none' for shared components only
    #[command(alias = "shell")]
    Env { name: Option<String> },
    /// Select Desktop or Laptop; use 'none' to clear the device profile
    Device {
        #[arg(value_parser = ["desktop", "laptop", "none"])]
        name: Option<String>,
    },
    /// Validate configuration and sources without running hooks or package managers
    Check {
        /// Check this local dotfiles checkout instead of the managed repository
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Check every environment/device combination
        #[arg(long)]
        all: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = AppConfig::new();
    if let Some(Commands::Check { repo, all }) = cli.command {
        return validation::check(&config, repo.as_deref(), all);
    }
    if let Some(Commands::Capture {
        repo,
        dry_run: true,
        yes,
        force,
    }) = cli.command
    {
        let (manifest, mut state) = load(&config)?;
        return capture::run(
            &config,
            &manifest,
            &mut state,
            repo.as_deref(),
            capture::CaptureOptions {
                dry_run: true,
                yes,
                force,
            },
        );
    }
    if matches!(cli.command, Some(Commands::Status)) {
        let (manifest, state) = load(&config)?;
        menu::show_status(&manifest, &state);
        return Ok(());
    }
    config.ensure_dirs()?;
    // Kernel-held lock releases on exit or crash; no stale PID file to remove.
    let _lock = lock(&config)?;
    match cli.command {
        Some(Commands::Init { url }) => cmd_init(&config, &url),
        Some(Commands::Update) => {
            ensure_initialized(&config)?;
            repo::pull_repo(&config.repo_dir)?;
            Manifest::load(&config.manifest_path())?;
            println!("Repository updated.");
            Ok(())
        }
        Some(Commands::Deploy {
            no_hooks,
            components,
        }) => {
            let (manifest, mut state) = load(&config)?;
            engine::deploy_installed(&config, &manifest, &mut state, &components, !no_hooks)
        }
        Some(Commands::Capture {
            repo,
            dry_run,
            yes,
            force,
        }) => {
            let (manifest, mut state) = load(&config)?;
            capture::run(
                &config,
                &manifest,
                &mut state,
                repo.as_deref(),
                capture::CaptureOptions {
                    dry_run,
                    yes,
                    force,
                },
            )
        }
        Some(Commands::Reinstall { components }) => {
            let (manifest, state) = load(&config)?;
            engine::reinstall_packages(&config, &manifest, &state, &components)
        }
        Some(Commands::Install { components }) => {
            let (manifest, mut state) = load(&config)?;
            engine::apply_changes(
                &config,
                &manifest,
                &mut state,
                &menu::ComponentSelection {
                    enabled: components,
                    disabled: Vec::new(),
                    skip_ask: true,
                },
            )
        }
        Some(Commands::Uninstall { components }) => {
            let (manifest, mut state) = load(&config)?;
            engine::apply_changes(
                &config,
                &manifest,
                &mut state,
                &menu::ComponentSelection {
                    enabled: Vec::new(),
                    disabled: components,
                    skip_ask: true,
                },
            )
        }
        Some(Commands::Sync) => {
            let (manifest, mut state) = load(&config)?;
            engine::sync_enabled(&config, &manifest, &mut state)
        }
        Some(Commands::Env { name }) => {
            let (manifest, mut state) = load(&config)?;
            let choice = if let Some(name) = name {
                let choice = (name != "none").then_some(name);
                validate_environment(&manifest, choice.as_deref())?;
                Some(choice)
            } else {
                require_terminal()?;
                menu::show_environment_menu(&manifest, state.active_environment.as_deref())?
            };
            if let Some(environment) = choice {
                let profile = Profile {
                    environment: environment.as_deref(),
                    device: state.active_device,
                };
                engine::change_profile(&config, &manifest, &mut state, profile)?;
            }
            Ok(())
        }
        Some(Commands::Device { name }) => {
            let (manifest, mut state) = load(&config)?;
            let choice = if let Some(name) = name {
                Some(match name.as_str() {
                    "desktop" => Some(Device::Desktop),
                    "laptop" => Some(Device::Laptop),
                    _ => None,
                })
            } else {
                require_terminal()?;
                menu::show_device_menu(state.active_device)?
            };
            if let Some(device) = choice {
                let environment = state.active_environment.clone();
                engine::change_profile(
                    &config,
                    &manifest,
                    &mut state,
                    Profile {
                        environment: environment.as_deref(),
                        device,
                    },
                )?;
            }
            Ok(())
        }
        None => {
            require_terminal()?;
            cmd_interactive(&config)
        }
        Some(Commands::Status | Commands::Check { .. }) => unreachable!(),
    }
}

fn lock(config: &AppConfig) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.state_dir.join("operation.lock"))?;
    file.try_lock()
        .context("Another Waysync operation is running")?;
    Ok(file)
}

fn require_terminal() -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("This action needs an interactive terminal. Use 'waysync status' or 'waysync check' for read-only output.");
    }
    Ok(())
}

fn validate_environment(manifest: &Manifest, environment: Option<&str>) -> Result<()> {
    if let Some(environment) = environment {
        if !manifest.available_environments().contains(&environment) {
            bail!("Unknown environment: {environment}");
        }
    }
    Ok(())
}

fn load(config: &AppConfig) -> Result<(Manifest, State)> {
    ensure_initialized(config)?;
    let manifest = Manifest::load(&config.manifest_path())?;
    let mut state = State::load(&config.state_file)?;
    state.migrate(&manifest);
    Ok((manifest, state))
}

fn cmd_init(config: &AppConfig, url: &str) -> Result<()> {
    if config.repo_dir.exists() {
        bail!(
            "Repository already initialized at {}",
            config.repo_dir.display()
        );
    }
    // Validate a staging checkout before making it the managed repository.
    let staging = tempfile::tempdir_in(&config.data_dir)?;
    let checkout = staging.path().join("repo");
    repo::clone_repo(url, &checkout)?;
    let manifest = Manifest::load(&checkout.join("manifest.toml"))?;
    std::fs::rename(&checkout, &config.repo_dir)?;
    let mut state = State {
        repo_url: Some(url.to_string()),
        ..State::default()
    };
    state.migrate(&manifest);
    state.save(&config.state_file)?;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        if !manifest.available_environments().is_empty() {
            if let Some(environment) = menu::show_environment_menu(&manifest, None)? {
                state.active_environment = environment;
            }
        }
        if let Some(device) = menu::show_device_menu(None)? {
            state.active_device = device;
        }
        state.save(&config.state_file)?;
    }
    println!("Repository initialized. Run 'waysync check' to inspect it, then 'waysync' to choose components.");
    Ok(())
}

fn cmd_interactive(config: &AppConfig) -> Result<()> {
    let (mut manifest, mut state) = load(config)?;
    let term = console::Term::stdout();
    let _screen = menu::ScreenGuard::enter(&term)?;
    let mut navigation = menu::Navigation::default();
    loop {
        let action = menu::show_main_menu(&manifest, state.profile(), &mut navigation)?;
        if matches!(action, MainMenuAction::Exit) {
            break;
        }
        if matches!(action, MainMenuAction::Status) {
            menu::show_status_menu(&manifest, &state)?;
            continue;
        }
        let visible = manifest.components_for_profile(state.profile());
        let selection = match action {
            MainMenuAction::ManageComponents => menu::show_components_menu(&visible, &state)?,
            MainMenuAction::InstallSpecific => {
                let available = visible
                    .iter()
                    .copied()
                    .filter(|c| !state.is_installed(&c.name))
                    .collect::<Vec<_>>();
                menu::show_install_menu(&available, &state)?
            }
            MainMenuAction::ReinstallPackages => {
                let installed = visible
                    .iter()
                    .copied()
                    .filter(|c| {
                        state.is_installed(&c.name)
                            && state.is_enabled(&c.name)
                            && !c.packages.is_empty()
                    })
                    .collect::<Vec<_>>();
                menu::show_reinstall_menu(&installed, &state)?
            }
            MainMenuAction::UninstallSpecific => {
                let installed = manifest
                    .components
                    .iter()
                    .filter(|c| state.is_installed(&c.name))
                    .collect::<Vec<_>>();
                menu::show_uninstall_menu(&installed, &state)?
            }
            _ => None,
        };
        if matches!(action, MainMenuAction::ReinstallPackages) {
            if let Some(selection) = selection {
                menu::with_visible_output(&term, || {
                    engine::reinstall_packages(config, &manifest, &state, &selection.enabled)
                })?;
            }
            continue;
        }
        if matches!(
            action,
            MainMenuAction::ManageComponents
                | MainMenuAction::InstallSpecific
                | MainMenuAction::UninstallSpecific
        ) {
            if let Some(selection) = selection {
                menu::with_visible_output(&term, || {
                    engine::apply_changes(config, &manifest, &mut state, &selection)
                })?;
            }
            continue;
        }
        match action {
            MainMenuAction::ChangeEnvironment => {
                if let Some(environment) =
                    menu::show_environment_menu(&manifest, state.active_environment.as_deref())?
                {
                    let device = state.active_device;
                    menu::with_visible_output(&term, || {
                        engine::change_profile(
                            config,
                            &manifest,
                            &mut state,
                            Profile {
                                environment: environment.as_deref(),
                                device,
                            },
                        )
                        .map(|_| ())
                    })?;
                }
            }
            MainMenuAction::ChangeDevice => {
                if let Some(device) = menu::show_device_menu(state.active_device)? {
                    let environment = state.active_environment.clone();
                    menu::with_visible_output(&term, || {
                        engine::change_profile(
                            config,
                            &manifest,
                            &mut state,
                            Profile {
                                environment: environment.as_deref(),
                                device,
                            },
                        )
                        .map(|_| ())
                    })?;
                }
            }
            MainMenuAction::UpdateRepo => {
                menu::with_visible_output(&term, || {
                    repo::pull_repo(&config.repo_dir)?;
                    manifest = Manifest::load(&config.manifest_path())?;
                    println!("Repository updated.");
                    Ok(())
                })?;
            }
            MainMenuAction::DeployDotfiles => {
                let run_hooks = menu::confirm_deploy_hooks()?;
                menu::with_visible_output(&term, || {
                    engine::deploy_installed(config, &manifest, &mut state, &[], run_hooks)
                })?;
            }
            MainMenuAction::CaptureChanges => {
                let checkout = menu::capture_checkout(state.working_repo.as_deref())?;
                menu::with_visible_output(&term, || {
                    capture::run(
                        config,
                        &manifest,
                        &mut state,
                        Some(&checkout),
                        capture::CaptureOptions::default(),
                    )
                })?;
            }
            MainMenuAction::Check => {
                menu::with_visible_output(&term, || validation::check(config, None, false))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn ensure_initialized(config: &AppConfig) -> Result<()> {
    if !config.manifest_path().is_file() {
        bail!("No repository configured. Run 'waysync init <url>' first.");
    }
    Ok(())
}

#[cfg(test)]
mod test_support;
