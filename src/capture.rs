use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::{Component as PathComponent, Path, PathBuf};

use anyhow::{bail, Context, Result};
use dialoguer::Confirm;

use crate::config::AppConfig;
use crate::manifest::Manifest;
use crate::state::State;
use crate::utils::{hash_contents, print_text_diff, read_file_contents, FileSource};

#[derive(Default)]
pub struct CaptureOptions {
    pub dry_run: bool,
    pub yes: bool,
    /// Allow overwriting a checkout file that also changed since deployment.
    pub force: bool,
}

struct Change {
    deployed: PathBuf,
    source: PathBuf,
    relative: PathBuf,
    sudo: bool,
    local: Vec<u8>,
    checkout: Vec<u8>,
    conflict: bool,
}

struct Plan {
    root: PathBuf,
    changes: Vec<Change>,
    skipped: usize,
}

fn safe_source(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, PathComponent::Normal(_)))
    {
        bail!("Invalid recorded source path: {}", relative.display());
    }
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            bail!(
                "Refusing to capture through a source symlink: {}",
                path.display()
            );
        }
    }
    if !path.is_file() || !path.canonicalize()?.starts_with(root) {
        bail!(
            "Capture source must be an existing file inside the checkout: {}",
            path.display()
        );
    }
    Ok(path)
}

fn plan_capture(
    config: &AppConfig,
    manifest: &Manifest,
    state: &State,
    checkout: &Path,
) -> Result<Plan> {
    let root = checkout
        .canonicalize()
        .with_context(|| format!("Cannot open authoring checkout: {}", checkout.display()))?;
    crate::repo::ensure_repo_root(&root)?;
    let target_config = AppConfig {
        repo_dir: root.clone(),
        state_dir: config.state_dir.clone(),
        data_dir: config.data_dir.clone(),
        state_file: config.state_file.clone(),
    };
    let target_manifest = Manifest::load(&target_config.manifest_path())?;
    let mut changes = Vec::new();
    let mut skipped = 0;
    let mut seen = HashSet::new();
    let mut destinations = HashSet::new();
    for component in manifest.components.iter().filter(|component| component.ask) {
        let Some(deployed) = state.deployed.get(&component.name) else {
            continue;
        };
        if deployed.is_empty() {
            continue;
        }
        let plans = (|| -> Result<_> {
            let target_component = target_manifest
                .find_component(&component.name)
                .with_context(|| {
                    format!(
                        "Component '{}' is absent from the authoring manifest",
                        component.name
                    )
                })?;
            let managed = crate::deploy::plan_component(component, config, state.profile())?;
            let target =
                crate::deploy::plan_component(target_component, &target_config, state.profile())?;
            Ok((managed, target))
        })();
        let (managed, target) = match plans {
            Ok(plans) => plans,
            Err(error) => {
                println!("  Skipping {}: {error:#}", component.name);
                skipped += deployed.len();
                continue;
            }
        };
        let managed: HashMap<_, _> = managed
            .iter()
            .map(|file| (file.dest.as_path(), file))
            .collect();
        let target: HashMap<_, _> = target
            .iter()
            .map(|file| (file.dest.as_path(), file))
            .collect();
        for key in deployed {
            if !seen.insert(key.clone()) {
                continue;
            }
            let deployed = Path::new(key);
            let change = (|| -> Result<Option<Change>> {
                let current = managed.get(deployed).context(
                    "No file entry matches this deployed path in the active manifest/profile",
                )?;
                let FileSource::Local(current_source) = &current.source else {
                    return Ok(None);
                };
                let Some(baseline) = state.hashes.get(key) else {
                    bail!("No deployment baseline; redeploy/review this file before capturing it");
                };
                // stat itself can fail on protected parent directories before sudo
                // gets a chance to read the file. Privileged entries go straight to sudo cat.
                if !current.entry.sudo && !deployed.try_exists()? {
                    bail!("Deployed file is missing; capture never propagates deletions");
                }
                let local = read_file_contents(deployed, current.entry.sudo)?;
                if hash_contents(&local) == *baseline {
                    return Ok(None);
                }
                let relative = state
                    .sources
                    .get(key)
                    .map(PathBuf::from)
                    .unwrap_or(current_source.strip_prefix(&config.repo_dir)?.to_path_buf());
                let source = safe_source(&root, &relative)?;
                let target = target
                    .get(deployed)
                    .context("No matching destination in the authoring checkout")?;
                let FileSource::Local(target_source) = &target.source else {
                    bail!("Authoring entry changed to an external repository");
                };
                if target_source.strip_prefix(&root)? != relative {
                    bail!("Override/source mapping changed (recorded {}); resolve the layer explicitly before capturing", relative.display());
                }
                let checkout = std::fs::read(&source)?;
                if checkout == local {
                    return Ok(None);
                }
                Ok(Some(Change {
                    deployed: deployed.to_path_buf(),
                    source,
                    relative,
                    sudo: current.entry.sudo,
                    conflict: hash_contents(&checkout) != *baseline,
                    local,
                    checkout,
                }))
            })();
            match change {
                Ok(Some(change)) => {
                    if !destinations.insert(change.source.clone()) {
                        bail!(
                            "Several deployed files map to the same authoring file: {}",
                            change.source.display()
                        );
                    }
                    changes.push(change);
                }
                Ok(None) => {}
                Err(error) => {
                    println!("  Skipping {}: {error:#}", deployed.display());
                    skipped += 1;
                }
            }
        }
    }
    changes.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(Plan {
        root,
        changes,
        skipped,
    })
}

fn show_change(change: &Change) {
    println!(
        "\n  {}{}",
        change.relative.display(),
        if change.conflict { " [conflict]" } else { "" }
    );
    if let (Ok(old), Ok(new)) = (
        std::str::from_utf8(&change.checkout),
        std::str::from_utf8(&change.local),
    ) {
        print_text_diff(old, new, "checkout", "deployed edit");
    } else {
        println!(
            "  Binary content differs: {} checkout bytes / {} deployed bytes",
            change.checkout.len(),
            change.local.len()
        );
    }
}

fn copy_change(root: &Path, change: &Change) -> Result<()> {
    let source = safe_source(root, &change.relative)?;
    if std::fs::read(&source)? != change.checkout
        || read_file_contents(&change.deployed, change.sudo)? != change.local
    {
        bail!(
            "File changed while reviewing {}; scan again",
            change.relative.display()
        );
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".waysync-capture-")
        .tempfile_in(source.parent().context("Source has no parent")?)?;
    // Keep repository permissions, rather than importing system-only modes (e.g. sudoers).
    temporary
        .as_file()
        .set_permissions(std::fs::metadata(&source)?.permissions())?;
    temporary.write_all(&change.local)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&source)
        .with_context(|| format!("Failed to capture {}", source.display()))?;
    Ok(())
}

/// Capture is separate from deployment: it never changes the deployment baseline,
/// installs packages, runs hooks, or stages/commits/pushes Git changes.
pub fn run(
    config: &AppConfig,
    manifest: &Manifest,
    state: &mut State,
    checkout: Option<&Path>,
    options: CaptureOptions,
) -> Result<()> {
    let checkout = checkout.or(state.working_repo.as_deref())
        .context("Choose an authoring checkout: waysync capture --repo ~/Projects/dotfiles (the path is remembered)")?;
    let plan = plan_capture(config, manifest, state, checkout)?;
    // Don't start copying and only then discover there is no terminal for approval.
    if !options.dry_run
        && !options.yes
        && !plan.changes.is_empty()
        && !std::io::stdin().is_terminal()
    {
        bail!("Review with --dry-run, then use --yes to copy nonconflicting changes without a terminal");
    }
    if !options.dry_run {
        state.working_repo = Some(plan.root.clone());
        state.save(&config.state_file)?;
    }
    let mut copied = 0;
    let mut skipped = plan.skipped;
    for change in &plan.changes {
        show_change(change);
        if options.dry_run {
            continue;
        }
        let approved = if options.yes {
            !change.conflict || options.force
        } else {
            Confirm::new()
                .with_prompt(if change.conflict {
                    "Overwrite checkout changes?"
                } else {
                    "Copy to checkout?"
                })
                .default(!change.conflict)
                .interact()?
        };
        if !approved {
            skipped += 1;
            println!("  Skipped.");
            continue;
        }
        copy_change(&plan.root, change)?;
        copied += 1;
    }
    if plan.changes.is_empty() && skipped == 0 {
        println!("No changes.");
    } else if options.dry_run {
        println!(
            "{} changed file{} (dry run).",
            plan.changes.len(),
            if plan.changes.len() == 1 { "" } else { "s" }
        );
    } else if copied == 0 {
        println!("No files captured.");
    } else {
        println!(
            "Captured {copied} file{}.",
            if copied == 1 { "" } else { "s" }
        );
    }
    if skipped > 0 {
        println!("{skipped} skipped.");
    }
    if skipped > 0 {
        bail!("Some files were skipped");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Device;
    use crate::test_support::{fixture, write};

    fn scenario() -> (tempfile::TempDir, AppConfig, PathBuf, Manifest, State) {
        let (root, config) = fixture();
        let working = root.path().join("authoring");
        std::fs::create_dir_all(&working).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&working)
            .status()
            .unwrap()
            .success());
        let text = format!(
            r#"
            [environments]
            available = ["shell"]
            [[components]]
            name = "editor"
            ask = true
            [[components.files]]
            src = "editor"
            dest = "{}"
            [[components]]
            name = "ignored"
            ask = false
            [[components.files]]
            src = "ignored"
            dest = "{}"
        "#,
            root.path().join("deployed/editor").display(),
            root.path().join("deployed/ignored").display()
        );
        for (relative, contents) in [
            ("manifest.toml", text.as_str()),
            ("dotfiles/editor/config", "base"),
            ("overrides/shell/editor/config", "environment"),
            ("devices/laptop/dotfiles/editor/config", "device"),
            ("devices/laptop/overrides/shell/editor/config", "combined"),
            ("dotfiles/editor/shared", "shared"),
            ("dotfiles/ignored", "ignored"),
        ] {
            write(config.repo_dir.join(relative), contents);
            write(working.join(relative), contents);
        }
        let manifest = Manifest::load(&config.manifest_path()).unwrap();
        let mut state = State {
            active_environment: Some("shell".into()),
            active_device: Some(Device::Laptop),
            ..State::default()
        };
        let environment = state.active_environment.clone();
        let profile = crate::profile::Profile {
            environment: environment.as_deref(),
            device: state.active_device,
        };
        for component in &manifest.components {
            state.components.insert(component.name.clone(), true);
            state.mark_installed(&component.name, true);
            crate::deploy::redeploy_component_files(component, &config, &mut state, profile)
                .unwrap();
        }
        state.save(&config.state_file).unwrap();
        (root, config, working, manifest, state)
    }

    #[test]
    fn captures_only_ask_components_to_the_original_layers_without_changing_baselines() {
        let (root, config, working, manifest, mut state) = scenario();
        write(
            root.path().join("deployed/editor/config"),
            "my laptop shell edit",
        );
        write(root.path().join("deployed/editor/shared"), "my shared edit");
        write(root.path().join("deployed/ignored"), "do not capture");
        let hashes = state.hashes.clone();
        let state_before = std::fs::read(&config.state_file).unwrap();
        run(
            &config,
            &manifest,
            &mut state,
            Some(&working),
            CaptureOptions {
                dry_run: true,
                ..CaptureOptions::default()
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(&config.state_file).unwrap(), state_before);
        assert!(state.working_repo.is_none());
        assert_eq!(
            std::fs::read_to_string(working.join("devices/laptop/overrides/shell/editor/config"))
                .unwrap(),
            "combined"
        );
        run(
            &config,
            &manifest,
            &mut state,
            Some(&working),
            CaptureOptions {
                yes: true,
                ..CaptureOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(working.join("devices/laptop/overrides/shell/editor/config"))
                .unwrap(),
            "my laptop shell edit"
        );
        assert_eq!(
            std::fs::read_to_string(working.join("dotfiles/editor/shared")).unwrap(),
            "my shared edit"
        );
        assert_eq!(
            std::fs::read_to_string(working.join("dotfiles/editor/config")).unwrap(),
            "base"
        );
        assert_eq!(
            std::fs::read_to_string(working.join("dotfiles/ignored")).unwrap(),
            "ignored"
        );
        assert_eq!(
            std::fs::read_to_string(
                config
                    .repo_dir
                    .join("devices/laptop/overrides/shell/editor/config")
            )
            .unwrap(),
            "combined"
        );
        assert_eq!(state.hashes, hashes);
        assert_eq!(
            State::load(&config.state_file).unwrap().working_repo,
            Some(working.clone())
        );
        // Already captured edits are not presented or written again; the path is remembered.
        assert!(plan_capture(&config, &manifest, &state, &working)
            .unwrap()
            .changes
            .is_empty());
        run(
            &config,
            &manifest,
            &mut state,
            None,
            CaptureOptions {
                yes: true,
                ..CaptureOptions::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn checkout_edits_are_not_overwritten_without_explicit_force() {
        let (root, config, working, manifest, mut state) = scenario();
        write(root.path().join("deployed/editor/shared"), "deployed edit");
        let target = working.join("dotfiles/editor/shared");
        write(&target, "independent checkout edit");
        assert!(run(
            &config,
            &manifest,
            &mut state,
            Some(&working),
            CaptureOptions {
                yes: true,
                ..CaptureOptions::default()
            }
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "independent checkout edit"
        );
        run(
            &config,
            &manifest,
            &mut state,
            Some(&working),
            CaptureOptions {
                yes: true,
                force: true,
                dry_run: false,
            },
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(target).unwrap(), "deployed edit");
    }

    #[test]
    fn missing_deployed_files_and_changed_layer_routes_are_never_guessed() {
        let (root, config, working, manifest, state) = scenario();
        std::fs::remove_file(root.path().join("deployed/editor/config")).unwrap();
        write(root.path().join("deployed/editor/shared"), "deployed edit");
        write(
            working.join("devices/laptop/overrides/shell/editor/shared"),
            "new override",
        );
        let plan = plan_capture(&config, &manifest, &state, &working).unwrap();
        assert_eq!(plan.skipped, 2);
        assert!(plan.changes.is_empty());
        assert_eq!(
            std::fs::read_to_string(working.join("dotfiles/editor/shared")).unwrap(),
            "shared"
        );
    }

    #[test]
    fn legacy_provenance_falls_back_to_manifest_and_concurrent_edits_abort_copy() {
        let (root, config, working, manifest, mut state) = scenario();
        state.sources.clear();
        write(root.path().join("deployed/editor/shared"), "deployed edit");
        let plan = plan_capture(&config, &manifest, &state, &working).unwrap();
        assert_eq!(plan.changes.len(), 1);
        write(working.join("dotfiles/editor/shared"), "edit during review");
        assert!(copy_change(&plan.root, &plan.changes[0]).is_err());
        assert_eq!(
            std::fs::read_to_string(working.join("dotfiles/editor/shared")).unwrap(),
            "edit during review"
        );
    }

    #[test]
    fn source_symlinks_cannot_write_outside_the_checkout() {
        let (root, config, working, manifest, mut state) = scenario();
        write(root.path().join("deployed/editor/shared"), "deployed edit");
        let outside = root.path().join("outside");
        write(&outside, "outside file");
        let target = working.join("dotfiles/editor/shared");
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(&outside, &target).unwrap();
        assert!(run(
            &config,
            &manifest,
            &mut state,
            Some(&working),
            CaptureOptions {
                yes: true,
                force: true,
                dry_run: false
            }
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside file");
    }
}

#[cfg(test)]
mod content_tests {
    use super::*;

    #[test]
    fn atomic_capture_supports_binary_content_and_keeps_repository_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let deployed = root.path().join("deployed");
        let source = root.path().join("source");
        let local = vec![0xff, 0x00, 0x01];
        std::fs::write(&deployed, &local).unwrap();
        std::fs::write(&source, b"old").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o750)).unwrap();
        let change = Change {
            deployed,
            source: source.clone(),
            relative: "source".into(),
            sudo: false,
            local: local.clone(),
            checkout: b"old".to_vec(),
            conflict: false,
        };
        show_change(&change);
        copy_change(root.path(), &change).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), local);
        assert_eq!(
            std::fs::metadata(source).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }
}
