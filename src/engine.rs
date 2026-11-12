use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use console::style;

use crate::config::AppConfig;
use crate::manifest::{Component, Manifest};
use crate::menu::{self, ComponentSelection};
use crate::profile::Profile;
use crate::state::State;
use crate::{deploy, hooks, packages};

/// Keep sudo alive without intercepting commands or changing PATH.
struct SudoSession {
    stop: Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for SudoSession {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn sudo_session(required: bool) -> Result<Option<SudoSession>> {
    if !required {
        return Ok(None);
    }
    let status = std::process::Command::new("sudo")
        .arg("-v")
        .status()
        .context("Failed to request administrator authentication")?;
    if !status.success() {
        bail!("Administrator authentication was not accepted");
    }
    let (stop, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        while matches!(
            receiver.recv_timeout(Duration::from_secs(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            let _ = std::process::Command::new("sudo")
                .args(["-n", "-v"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    });
    Ok(Some(SudoSession {
        stop,
        worker: Some(worker),
    }))
}

fn requires_sudo(component: &Component) -> bool {
    !component.packages.is_empty()
        || component.files.iter().any(|file| file.sudo)
        || component
            .post_install
            .iter()
            .chain(&component.post_deploy)
            .any(|hook| {
                hook.cmd()
                    .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                    .any(|token| matches!(token, "sudo" | "hyprpm" | "pkexec"))
            })
}

fn retained_packages(
    manifest: &Manifest,
    config: &AppConfig,
    names: &HashSet<&str>,
) -> Result<HashSet<String>> {
    let mut retained = HashSet::new();
    for component in manifest
        .components
        .iter()
        .filter(|c| names.contains(c.name.as_str()))
    {
        for name in packages::package_names_for_specs(&component.packages, &config.repo_dir)? {
            if !name.contains(['<', '>', '=']) {
                retained.insert(
                    name.strip_suffix("-git")
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("{name}-git")),
                );
            }
            retained.insert(name);
        }
    }
    Ok(retained)
}

/// Batch package removal so dependencies between modules removed together do not
/// depend on manifest order. File ownership and partial progress remain per module.
fn remove_components(
    components: &[&Component],
    config: &AppConfig,
    state: &mut State,
    retained: &HashSet<String>,
) -> Result<()> {
    let mut candidates = HashSet::new();
    for component in components {
        for name in packages::package_names_for_specs(&component.packages, &config.repo_dir)? {
            if !retained.contains(&name) {
                candidates.insert(name);
            } else {
                println!("  Keeping {name}: claimed by a retained component.");
            }
        }
    }
    packages::uninstall_package_names(&candidates.into_iter().collect::<Vec<_>>())?;
    for component in components {
        println!("\n  {} {}", style("[-]").red(), component.name);
        deploy::undeploy_component_files(component, state)?;
        state.mark_installed(&component.name, false);
        state.save(&config.state_file)?;
    }
    Ok(())
}

fn install_component(component: &Component, config: &AppConfig, state: &mut State) -> Result<()> {
    println!("\n  {} {}", style("[+]").green(), component.name);
    let newly_enabled = !state.is_installed(&component.name);
    if newly_enabled && !component.packages.is_empty() {
        let missing = packages::missing_specs(&component.packages, &config.repo_dir)?;
        if !missing.is_empty() {
            packages::ensure_paru()?;
        }
        let mut failures = Vec::new();
        for spec in missing {
            if let Err(error) =
                packages::install_packages(std::slice::from_ref(&spec), &config.repo_dir, true)
            {
                failures.push(format!("{spec}: {error:#}"));
            }
        }
        if !failures.is_empty() {
            bail!("Package installation incomplete: {}", failures.join("; "));
        }
    }
    if newly_enabled {
        state.pending_install.insert(component.name.clone());
    }
    // Remember successful package work even if the subsequent deployment fails.
    state.mark_installed(&component.name, true);
    let environment = state.active_environment.clone();
    let profile = Profile {
        environment: environment.as_deref(),
        device: state.active_device,
    };
    deploy::redeploy_component_files(component, config, state, profile)?;
    // Installation hooks often restart services that need the deployed config first.
    if state.pending_install.contains(&component.name) {
        hooks::run_hooks(
            &component.post_install,
            "post_install",
            profile,
            &config.repo_dir,
        )?;
        state.pending_install.remove(&component.name);
    }
    hooks::run_hooks(
        &component.post_deploy,
        "post_deploy",
        profile,
        &config.repo_dir,
    )?;
    Ok(())
}

/// Always checkpoint partial progress. Files and package managers aren't transactional.
fn checkpoint<T>(config: &AppConfig, state: &State, result: Result<T>) -> Result<T> {
    let saved = state.save(&config.state_file);
    match result {
        Ok(value) => {
            saved?;
            Ok(value)
        }
        Err(error) => {
            if let Err(save_error) = saved {
                eprintln!("Could not save partial progress: {save_error:#}");
            }
            Err(error)
        }
    }
}

/// Only installed, still-selected components need repair auditing. New components
/// use the normal installation path; dotfile-only deployment never calls this.
fn audit_installed_packages<'a>(
    config: &AppConfig,
    components: impl IntoIterator<Item = &'a Component>,
    state: &State,
) -> Result<Vec<(&'a Component, Vec<String>)>> {
    let components: Vec<_> = components
        .into_iter()
        .filter(|c| state.is_installed(&c.name) && !c.packages.is_empty())
        .collect();
    if components.is_empty() {
        return Ok(Vec::new());
    }
    let installed = packages::installed_package_names()?;
    let mut resolved = HashMap::new();
    let mut repairs = Vec::new();
    for component in components {
        let mut missing = Vec::new();
        for spec in &component.packages {
            if !resolved.contains_key(spec) {
                resolved.insert(
                    spec.clone(),
                    packages::names_for_package_audit(spec, &config.repo_dir)?,
                );
            }
            let names = &resolved[spec];
            let mut satisfied = true;
            for name in names {
                if !packages::requirement_satisfied(name, &installed)? {
                    satisfied = false;
                }
            }
            if !satisfied {
                missing.push(spec.clone());
            }
        }
        if !missing.is_empty() {
            repairs.push((component, missing));
        }
    }
    Ok(repairs)
}

#[derive(Default)]
struct RepairReport {
    failed_components: HashSet<String>,
    errors: Vec<String>,
}

fn finish_with_issues(errors: &[String]) -> Result<()> {
    if errors.is_empty() {
        return Ok(());
    }
    eprintln!("\nCompleted remaining work with {} issue(s):", errors.len());
    for error in errors {
        eprintln!("  - {error}");
    }
    bail!("Some component steps failed; successful work was saved and failed work can be retried with sync")
}

fn repair_missing_packages(
    config: &AppConfig,
    repairs: &[(&Component, Vec<String>)],
) -> RepairReport {
    let mut attempted: HashMap<String, bool> = HashMap::new();
    let mut report = RepairReport::default();
    for (component, specs) in repairs {
        for spec in specs {
            let failed = if let Some(failed) = attempted.get(spec) {
                *failed
            } else {
                println!("  Restoring missing package for {}: {spec}", component.name);
                let result = packages::ensure_paru().and_then(|_| {
                    packages::install_packages(std::slice::from_ref(spec), &config.repo_dir, true)
                });
                let failed = result.is_err();
                if let Err(error) = result {
                    let message = format!("{} ({spec}): {error:#}", component.name);
                    eprintln!("  Skipping failed repair: {message}");
                    report.errors.push(message);
                }
                attempted.insert(spec.clone(), failed);
                failed
            };
            if failed {
                report.failed_components.insert(component.name.clone());
            }
        }
    }
    report
}

pub fn apply_changes(
    config: &AppConfig,
    manifest: &Manifest,
    state: &mut State,
    selection: &ComponentSelection,
) -> Result<()> {
    for name in &selection.disabled {
        if manifest.find_component(name).is_none() {
            bail!("Unknown component: {name}");
        }
        if selection.enabled.contains(name) {
            bail!("Component '{name}' cannot be installed and uninstalled together");
        }
    }
    let mut candidates = Vec::new();
    for name in &selection.enabled {
        let component = manifest
            .find_component(name)
            .with_context(|| format!("Unknown component: {name}"))?;
        if !component.applies_to(state.profile()) {
            bail!("Component '{name}' does not apply to the active profile");
        }
        candidates.push(component);
    }
    let repairs = audit_installed_packages(config, candidates, state)?;
    let mut selected = Vec::new();
    for name in &selection.enabled {
        let component = manifest
            .find_component(name)
            .with_context(|| format!("Unknown component: {name}"))?;
        if !component.applies_to(state.profile()) {
            bail!("Component '{name}' does not apply to the active profile");
        }
        if component.ask
            && state.is_installed(name)
            && !selection.skip_ask
            && !menu::confirm_component_update(name)?
        {
            continue;
        }
        selected.push(component);
    }
    let mut final_names: HashSet<_> = manifest
        .components_for_profile(state.profile())
        .into_iter()
        .filter(|c| state.is_installed(&c.name))
        .map(|c| c.name.as_str())
        .collect();
    for name in &selection.disabled {
        final_names.remove(name.as_str());
    }
    final_names.extend(selected.iter().map(|c| c.name.as_str()));
    if !selected.is_empty() {
        deploy::preflight_files(manifest, config, state.profile(), &final_names)?;
    }
    let installing = selected
        .iter()
        .filter(|c| !state.is_installed(&c.name))
        .map(|c| c.name.as_str())
        .collect();
    deploy::preflight(manifest, config, state.profile(), &installing)?;
    let removals: Vec<_> = selection
        .disabled
        .iter()
        .filter(|name| state.is_installed(name))
        .filter_map(|name| manifest.find_component(name))
        .collect();
    let retained = if removals.is_empty() {
        HashSet::new()
    } else {
        retained_packages(manifest, config, &final_names)?
    };
    let _sudo = sudo_session(
        !repairs.is_empty()
            || selected.iter().any(|c| apply_requires_sudo(c, state))
            || removals.iter().any(|c| requires_sudo(c)),
    )?;
    let mut report = repair_missing_packages(config, &repairs);
    if !selection.disabled.is_empty() {
        for name in &selection.disabled {
            state.components.insert(name.clone(), false);
        }
        state.save(&config.state_file)?;
        let result = remove_components(&removals, config, state, &retained);
        checkpoint(config, state, result)?;
    }
    for component in selected {
        state.components.insert(component.name.clone(), true);
        if report.failed_components.contains(&component.name) {
            state.save(&config.state_file)?;
            println!(
                "  Deferring files/hooks for {} until its missing packages are restored.",
                component.name
            );
            continue;
        }
        let result = install_component(component, config, state);
        state.save(&config.state_file)?;
        if let Err(error) = result {
            report.errors.push(format!("{}: {error:#}", component.name));
        }
    }
    finish_with_issues(&report.errors)
}

/// Deploy only enabled, installed components. Never install/remove packages or run post_install.
pub fn deploy_installed(
    config: &AppConfig,
    manifest: &Manifest,
    state: &mut State,
    names: &[String],
    run_hooks: bool,
) -> Result<()> {
    let components = installed_targets(manifest, state, names)?;
    let selected: HashSet<_> = components.iter().map(|c| c.name.as_str()).collect();
    deploy::preflight_files(manifest, config, state.profile(), &selected)?;
    let _sudo = sudo_session(
        components
            .iter()
            .any(|c| deployment_requires_sudo(c, state.profile(), run_hooks)),
    )?;
    for component in components {
        if component.ask && names.is_empty() && !menu::confirm_component_update(&component.name)? {
            continue;
        }
        let environment = state.active_environment.clone();
        let profile = Profile {
            environment: environment.as_deref(),
            device: state.active_device,
        };
        let result = (|| -> Result<()> {
            deploy::redeploy_component_files(component, config, state, profile)?;
            if run_hooks {
                hooks::run_hooks(
                    &component.post_deploy,
                    "post_deploy",
                    profile,
                    &config.repo_dir,
                )?;
            }
            Ok(())
        })();
        checkpoint(config, state, result)?;
    }
    Ok(())
}

fn installed_targets<'a>(
    manifest: &'a Manifest,
    state: &State,
    names: &[String],
) -> Result<Vec<&'a Component>> {
    for name in names {
        let component = manifest
            .find_component(name)
            .with_context(|| format!("Unknown component: {name}"))?;
        if !component.applies_to(state.profile())
            || !state.is_installed(name)
            || !state.is_enabled(name)
        {
            bail!("Component '{name}' is not installed and enabled in the active profile");
        }
    }
    Ok(manifest
        .components_for_profile(state.profile())
        .into_iter()
        .filter(|c| {
            state.is_installed(&c.name)
                && state.is_enabled(&c.name)
                && (names.is_empty() || names.contains(&c.name))
        })
        .collect())
}

/// Force package reinstall/rebuild and run reload hooks; files and selections are untouched.
pub fn reinstall_packages(
    config: &AppConfig,
    manifest: &Manifest,
    state: &State,
    names: &[String],
) -> Result<()> {
    let components: Vec<_> = installed_targets(manifest, state, names)?
        .into_iter()
        .filter(|c| !c.packages.is_empty())
        .collect();
    let _sudo = sudo_session(!components.is_empty())?;
    let mut errors = Vec::new();
    for component in components {
        let failures_before = errors.len();
        for spec in &component.packages {
            if let Err(error) = packages::ensure_paru().and_then(|_| {
                packages::install_packages(std::slice::from_ref(spec), &config.repo_dir, false)
            }) {
                errors.push(format!("{} ({spec}): {error:#}", component.name));
            }
        }
        if errors.len() == failures_before {
            let reload = if !component.post_reinstall.is_empty() {
                &component.post_reinstall
            } else if component.category.as_deref() == Some("shell") {
                &component.post_deploy
            } else {
                continue;
            };
            if let Err(error) =
                hooks::run_hooks(reload, "post_reinstall", state.profile(), &config.repo_dir)
            {
                errors.push(format!("{} reload: {error:#}", component.name));
            }
        }
    }
    finish_with_issues(&errors)
}

fn deployment_requires_sudo(component: &Component, profile: Profile<'_>, run_hooks: bool) -> bool {
    component
        .files
        .iter()
        .any(|f| f.sudo && f.applies_to(profile))
        || (run_hooks
            && component
                .post_deploy
                .iter()
                .filter(|h| h.applies_to(profile))
                .any(|h| hook_requires_sudo(h.cmd())))
}

fn hook_requires_sudo(command: &str) -> bool {
    command
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .any(|token| matches!(token, "sudo" | "hyprpm" | "pkexec"))
}

fn apply_requires_sudo(component: &Component, state: &State) -> bool {
    if !state.is_installed(&component.name) {
        return requires_sudo(component);
    }
    deployment_requires_sudo(component, state.profile(), true)
        || (state.pending_install.contains(&component.name)
            && component
                .post_install
                .iter()
                .filter(|h| h.applies_to(state.profile()))
                .any(|h| hook_requires_sudo(h.cmd())))
}

/// Shared full-sync entry point for CLI Sync and Manage → Apply.
pub fn full_sync(
    config: &AppConfig,
    manifest: &Manifest,
    state: &mut State,
    selection: Option<&ComponentSelection>,
) -> Result<()> {
    let active: HashSet<_> = match selection {
        Some(selection) => selection.enabled.iter().map(String::as_str).collect(),
        None => manifest
            .components_for_profile(state.profile())
            .into_iter()
            .filter(|c| state.is_enabled(&c.name))
            .map(|c| c.name.as_str())
            .collect(),
    };
    for name in &active {
        let component = manifest
            .find_component(name)
            .with_context(|| format!("Unknown component: {name}"))?;
        if !component.applies_to(state.profile()) {
            bail!("Component '{name}' is inactive");
        }
    }
    deploy::preflight(manifest, config, state.profile(), &active)?;
    if manifest
        .components
        .iter()
        .any(|c| active.contains(c.name.as_str()) && !c.packages.is_empty())
    {
        packages::installed_package_names()?;
    }
    let mut local_specs: Vec<_> = manifest
        .components
        .iter()
        .filter(|c| active.contains(c.name.as_str()))
        .flat_map(|c| c.packages.iter())
        .filter(|spec| spec.starts_with("local:"))
        .cloned()
        .collect();
    local_specs.sort();
    local_specs.dedup();
    // Custom recipes must not accidentally be upgraded from an AUR recipe of the same name.
    let mut ignored = Vec::new();
    for spec in &local_specs {
        ignored.extend(packages::names_for_package_audit(spec, &config.repo_dir)?);
    }
    ignored.sort();
    ignored.dedup();
    let _sudo = sudo_session(true)?;
    let mut errors = packages::upgrade_system(&ignored)?;
    for spec in local_specs {
        if let Err(error) = packages::update_local_package(&spec, &config.repo_dir) {
            errors.push(format!("{spec}: {error:#}"));
        }
    }
    let result = match selection {
        Some(selection) => apply_changes(config, manifest, state, selection),
        None => sync_enabled(config, manifest, state),
    };
    if let Err(error) = result {
        errors.push(format!("Components: {error:#}"));
    }
    finish_with_issues(&errors)
}

pub fn sync_enabled(config: &AppConfig, manifest: &Manifest, state: &mut State) -> Result<()> {
    // Retry unfinished cleanup from a profile switch or a previous failed removal.
    let active: HashSet<_> = manifest
        .components_for_profile(state.profile())
        .into_iter()
        .filter(|c| state.is_enabled(&c.name))
        .map(|c| c.name.as_str())
        .collect();
    deploy::preflight_files(manifest, config, state.profile(), &active)?;
    let inactive: Vec<_> = manifest
        .components
        .iter()
        .filter(|c| state.is_installed(&c.name) && !active.contains(c.name.as_str()))
        .collect();
    let retained = if inactive.is_empty() {
        HashSet::new()
    } else {
        retained_packages(manifest, config, &active)?
    };
    let _sudo = sudo_session(inactive.iter().any(|c| requires_sudo(c)))?;
    let result = remove_components(&inactive, config, state, &retained);
    checkpoint(config, state, result)?;
    let selection = ComponentSelection {
        enabled: manifest
            .components
            .iter()
            .filter(|c| active.contains(c.name.as_str()))
            .map(|c| c.name.clone())
            .collect(),
        disabled: Vec::new(),
        skip_ask: false,
    };
    apply_changes(config, manifest, state, &selection)
}

/// A profile switch only redeploys still-active components whose effective inputs change.
pub fn affected(
    component: &Component,
    config: &AppConfig,
    old: Profile<'_>,
    new: Profile<'_>,
) -> Result<bool> {
    let old_plan = match deploy::plan_component(component, config, old) {
        Ok(plan) => plan,
        Err(_) => return Ok(true),
    };
    let new_plan = deploy::plan_component(component, config, new)?;
    let signature = |plan: Vec<deploy::PlannedFile<'_>>| -> Vec<_> {
        plan.into_iter()
            .map(|file| {
                let source = match file.source {
                    crate::utils::FileSource::Local(path) => path.to_string_lossy().into_owned(),
                    crate::utils::FileSource::Repo(url) => url,
                };
                (file.dest, source)
            })
            .collect()
    };
    Ok(signature(old_plan) != signature(new_plan)
        || component
            .post_deploy
            .iter()
            .any(|hook| hook.is_scoped() && hook.applies_to(new) && !hook.applies_to(old)))
}

pub fn change_profile(
    config: &AppConfig,
    manifest: &Manifest,
    state: &mut State,
    new: Profile<'_>,
) -> Result<bool> {
    let old_environment = state.active_environment.clone();
    let old = Profile {
        environment: old_environment.as_deref(),
        device: state.active_device,
    };
    if old == new {
        println!("Profile is already active.");
        return Ok(false);
    }
    let newly_visible: Vec<_> = manifest
        .components_for_profile(new)
        .into_iter()
        .filter(|c| !c.applies_to(old))
        .collect();
    let selection = if newly_visible.is_empty() {
        None
    } else {
        let Some(selection) = menu::show_components_menu(&newly_visible, state)? else {
            return Ok(false);
        };
        Some(selection)
    };
    let mut final_names: HashSet<_> = manifest
        .components_for_profile(new)
        .into_iter()
        .filter(|c| state.is_enabled(&c.name))
        .map(|c| c.name.as_str())
        .collect();
    if let Some(selection) = &selection {
        for name in &selection.disabled {
            final_names.remove(name.as_str());
        }
        final_names.extend(selection.enabled.iter().map(String::as_str));
    }
    deploy::preflight_files(manifest, config, new, &final_names)?;
    let installing = manifest
        .components
        .iter()
        .filter(|c| final_names.contains(c.name.as_str()) && !state.is_installed(&c.name))
        .map(|c| c.name.as_str())
        .collect();
    deploy::preflight(manifest, config, new, &installing)?;
    let repairs = audit_installed_packages(
        config,
        manifest
            .components
            .iter()
            .filter(|c| final_names.contains(c.name.as_str())),
        state,
    )?;
    let removals: Vec<_> = manifest
        .components
        .iter()
        .filter(|c| state.is_installed(&c.name) && !final_names.contains(c.name.as_str()))
        .collect();
    let mut updates = Vec::new();
    for component in manifest
        .components_for_profile(new)
        .into_iter()
        .filter(|c| final_names.contains(c.name.as_str()))
    {
        let newly_active = !component.applies_to(old) || !state.is_installed(&component.name);
        if newly_active || affected(component, config, old, new)? {
            // A declined optional update stays pending for the next explicit sync.
            if !newly_active && component.ask && !menu::confirm_component_update(&component.name)? {
                continue;
            }
            updates.push((component, newly_active));
        }
    }
    let _sudo = sudo_session(
        !repairs.is_empty()
            || removals.iter().any(|c| requires_sudo(c))
            || updates.iter().any(|(c, newly_active)| {
                if *newly_active {
                    requires_sudo(c)
                } else {
                    deployment_requires_sudo(c, new, true)
                }
            }),
    )?;
    let retained = if removals.is_empty() {
        HashSet::new()
    } else {
        retained_packages(manifest, config, &final_names)?
    };
    // Persist the requested profile before side effects; update can resume a partial switch.
    state.active_environment = new.environment.map(String::from);
    state.active_device = new.device;
    if let Some(selection) = &selection {
        for name in &selection.enabled {
            state.components.insert(name.clone(), true);
        }
        for name in &selection.disabled {
            state.components.insert(name.clone(), false);
        }
    }
    state.save(&config.state_file)?;
    let mut report = repair_missing_packages(config, &repairs);
    let result = remove_components(&removals, config, state, &retained);
    checkpoint(config, state, result)?;
    for (component, newly_active) in updates {
        if report.failed_components.contains(&component.name) {
            continue;
        }
        let result = if newly_active {
            install_component(component, config, state)
        } else {
            (|| -> Result<()> {
                deploy::redeploy_component_files(component, config, state, new)?;
                hooks::run_hooks(&component.post_deploy, "post_deploy", new, &config.repo_dir)
            })()
        };
        state.save(&config.state_file)?;
        if let Err(error) = result {
            report.errors.push(format!("{}: {error:#}", component.name));
        }
    }
    finish_with_issues(&report.errors)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Device;
    use crate::test_support::{fixture, write};

    fn selection() -> ComponentSelection {
        ComponentSelection {
            enabled: vec!["config".into()],
            disabled: vec![],
            skip_ask: true,
        }
    }

    #[test]
    fn device_switch_updates_enabled_environment_component_and_only_affected_globals() {
        let (root, config) = fixture();
        write(config.dotfiles_dir().join("config"), "base");
        write(
            config.repo_dir.join("devices/laptop/dotfiles/config"),
            "laptop",
        );
        let manifest: Manifest = toml::from_str(&format!(
            r#"
            [environments]
            available = ["shell"]
            [[components]]
            name = "config"
            environments = ["shell"]
            [[components.files]]
            src = "config"
            dest = "{}"
            [[components]]
            name = "unrelated"
        "#,
            root.path().join("dest").display()
        ))
        .unwrap();
        let mut state = State {
            active_environment: Some("shell".into()),
            ..State::default()
        };
        apply_changes(&config, &manifest, &mut state, &selection()).unwrap();
        assert!(!affected(
            &manifest.components[1],
            &config,
            state.profile(),
            Profile {
                environment: Some("shell"),
                device: Some(Device::Laptop)
            }
        )
        .unwrap());
        assert!(change_profile(
            &config,
            &manifest,
            &mut state,
            Profile {
                environment: Some("shell"),
                device: Some(Device::Laptop)
            }
        )
        .unwrap());
        assert_eq!(
            std::fs::read_to_string(root.path().join("dest")).unwrap(),
            "laptop"
        );
        assert_eq!(
            State::load(&config.state_file).unwrap().active_device,
            Some(Device::Laptop)
        );
        change_profile(
            &config,
            &manifest,
            &mut state,
            Profile {
                environment: Some("shell"),
                device: Some(Device::Desktop),
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("dest")).unwrap(),
            "base"
        );
    }

    #[test]
    fn failed_hook_checkpoints_files_and_retries_post_install_after_deployment() {
        let (root, config) = fixture();
        write(config.dotfiles_dir().join("config"), "config");
        let dest = root.path().join("dest");
        let marker = root.path().join("ready");
        let manifest: Manifest = toml::from_str(&format!(
            r#"
            [[components]]
            name = "config"
            post_install = ["test -f '{}' && test -f '{}' "]
            [[components.files]]
            src = "config"
            dest = "{}"
        "#,
            dest.display(),
            marker.display(),
            dest.display()
        ))
        .unwrap();
        let mut state = State::default();
        assert!(apply_changes(&config, &manifest, &mut state, &selection()).is_err());
        let saved = State::load(&config.state_file).unwrap();
        assert!(saved.is_installed("config"));
        assert!(saved.pending_install.contains("config"));
        assert_eq!(saved.deployed["config"].len(), 1);
        write(marker, "ready");
        sync_enabled(&config, &manifest, &mut state).unwrap();
        assert!(state.pending_install.is_empty());
    }

    #[test]
    fn invalid_plan_stops_before_any_file_or_state_changes() {
        let (root, config) = fixture();
        let manifest: Manifest = toml::from_str(&format!(
            "[[components]]\nname = 'config'\n[[components.files]]\nsrc = 'missing'\ndest = '{}'",
            root.path().join("dest").display()
        ))
        .unwrap();
        let mut state = State::default();
        assert!(apply_changes(&config, &manifest, &mut state, &selection()).is_err());
        assert!(!root.path().join("dest").exists());
        assert!(!config.state_file.exists());
        assert!(state.components.is_empty());
    }
}
