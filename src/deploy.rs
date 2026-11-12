use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use console::style;
use dialoguer::Select;

use crate::config::AppConfig;
use crate::manifest::{Component, FileEntry, Manifest};
use crate::profile::Profile;
use crate::state::State;
use crate::utils::{
    collect_directory_files, copy_file, destination_path, hash_contents, print_text_diff,
    read_file_contents, remove_path, resolve_file_source, FileSource,
};

pub struct PlannedFile<'a> {
    pub source: FileSource,
    pub dest: PathBuf,
    pub entry: &'a FileEntry,
}

/// Resolve everything before touching packages, hooks, or destinations.
pub fn plan_component<'a>(
    component: &'a Component,
    config: &AppConfig,
    profile: Profile<'_>,
) -> Result<Vec<PlannedFile<'a>>> {
    let mut planned = Vec::new();
    for entry in component
        .files
        .iter()
        .filter(|file| file.applies_to(profile))
    {
        let dest = destination_path(&entry.dest)?;
        for protected in [&config.repo_dir, &config.state_dir] {
            if dest.starts_with(protected) || protected.starts_with(&dest) {
                bail!(
                    "Destination overlaps Waysync's own files: {}",
                    dest.display()
                );
            }
        }
        match resolve_file_source(&entry.src, config, profile) {
            FileSource::Repo(url) => planned.push(PlannedFile {
                source: FileSource::Repo(url),
                dest,
                entry,
            }),
            FileSource::Local(source) => {
                let metadata = std::fs::symlink_metadata(&source).with_context(|| {
                    format!(
                        "Component '{}': source not found: {}",
                        component.name,
                        source.display()
                    )
                })?;
                if !source
                    .canonicalize()?
                    .starts_with(config.repo_dir.canonicalize()?)
                {
                    bail!("Source escapes the dotfiles checkout: {}", source.display());
                }
                if metadata.file_type().is_symlink() {
                    bail!("Symlink sources are unsupported: {}", source.display());
                }
                if metadata.is_dir() {
                    for (source, relative) in collect_directory_files(&entry.src, config, profile)?
                    {
                        planned.push(PlannedFile {
                            source: FileSource::Local(source),
                            dest: dest.join(relative),
                            entry,
                        });
                    }
                } else if metadata.is_file() {
                    planned.push(PlannedFile {
                        source: FileSource::Local(source),
                        dest,
                        entry,
                    });
                } else {
                    bail!(
                        "Source must be a regular file or directory: {}",
                        source.display()
                    );
                }
            }
        }
    }
    Ok(planned)
}

pub fn preflight(
    manifest: &Manifest,
    config: &AppConfig,
    profile: Profile<'_>,
    names: &HashSet<&str>,
) -> Result<()> {
    preflight_inner(manifest, config, profile, names, true)
}

pub fn preflight_files(
    manifest: &Manifest,
    config: &AppConfig,
    profile: Profile<'_>,
    names: &HashSet<&str>,
) -> Result<()> {
    preflight_inner(manifest, config, profile, names, false)
}

fn preflight_inner(
    manifest: &Manifest,
    config: &AppConfig,
    profile: Profile<'_>,
    names: &HashSet<&str>,
    check_packages: bool,
) -> Result<()> {
    let mut destinations: HashMap<PathBuf, &str> = HashMap::new();
    for component in manifest
        .components_for_profile(profile)
        .into_iter()
        .filter(|c| names.contains(c.name.as_str()))
    {
        for file in plan_component(component, config, profile)? {
            for (other, owner) in &destinations {
                if file.dest.starts_with(other) || other.starts_with(&file.dest) {
                    bail!(
                        "Conflicting destinations for '{}' and '{}': {} and {}",
                        owner,
                        component.name,
                        other.display(),
                        file.dest.display()
                    );
                }
            }
            destinations.insert(file.dest, &component.name);
        }
        for spec in component.packages.iter().filter(|_| check_packages) {
            if let Some(name) = spec.strip_prefix("local:") {
                let recipe = config.repo_dir.join("packages").join(name).join("PKGBUILD");
                if !recipe.is_file() {
                    bail!("Missing local PKGBUILD: {}", recipe.display());
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum FileDeployment {
    Written,
    KeptManaged,
    KeptUnmanaged,
}

/// Track each completed file immediately; callers checkpoint state even on error.
pub fn redeploy_component_files(
    component: &Component,
    config: &AppConfig,
    state: &mut State,
    profile: Profile<'_>,
) -> Result<()> {
    let planned = plan_component(component, config, profile)?;
    let old = state
        .deployed
        .get(&component.name)
        .cloned()
        .unwrap_or_default();
    let mut desired = HashSet::new();
    for file in planned {
        let key = file.dest.to_string_lossy().into_owned();
        desired.insert(key.clone());
        let owned = match file.source {
            FileSource::Repo(url) => {
                if file.dest.exists() {
                    crate::repo::pull_repo(&file.dest)?;
                } else {
                    crate::repo::clone_repo(&url, &file.dest)?;
                }
                // External checkouts can contain user work; never recursively remove them.
                true
            }
            FileSource::Local(source) => {
                let outcome = deploy_single_file(&source, &file.dest, file.entry, state)?;
                if outcome == FileDeployment::Written {
                    let relative = source.strip_prefix(&config.repo_dir)?;
                    state
                        .sources
                        .insert(key.clone(), relative.to_string_lossy().into_owned());
                }
                outcome != FileDeployment::KeptUnmanaged
            }
        };
        if owned {
            let tracked = state.deployed.entry(component.name.clone()).or_default();
            if !tracked.contains(&key) {
                tracked.push(key);
            }
        }
    }
    for orphan in old.iter().filter(|path| !desired.contains(*path)) {
        release_file(component, orphan, state)?;
    }
    Ok(())
}

fn deploy_single_file(
    src: &Path,
    dest: &Path,
    entry: &FileEntry,
    state: &mut State,
) -> Result<FileDeployment> {
    deploy_single_file_with_choice(src, dest, entry, state, || {
        Select::new()
            .with_prompt("Resolve file conflict")
            .items(&[
                "Keep local version",
                "Overwrite with incoming version",
                "Keep local, save incoming as .new",
            ])
            .default(0)
            .interact()
            .map_err(Into::into)
    })
}

fn deploy_single_file_with_choice(
    src: &Path,
    dest: &Path,
    entry: &FileEntry,
    state: &mut State,
    choice: impl FnOnce() -> Result<usize>,
) -> Result<FileDeployment> {
    let incoming = std::fs::read(src)?;
    let incoming_hash = hash_contents(&incoming);
    let key = dest.to_string_lossy().into_owned();
    if dest.exists() && entry.check {
        let local = read_file_contents(dest, entry.sudo)?;
        let local_hash = hash_contents(&local);
        if local_hash == incoming_hash {
            state.hashes.insert(key, incoming_hash);
            return Ok(FileDeployment::Written);
        }
        if state.hashes.get(&key) != Some(&local_hash) {
            println!(
                "\n  {} differs from the last deployed version",
                style(dest.display()).yellow()
            );
            if let (Ok(local), Ok(incoming)) =
                (std::str::from_utf8(&local), std::str::from_utf8(&incoming))
            {
                print_text_diff(local, incoming, "local", "incoming");
            } else {
                println!(
                    "  Binary content differs ({} local / {} incoming bytes).",
                    local.len(),
                    incoming.len()
                );
            }
            let selection = choice()?;
            if selection != 1 {
                if selection == 2 {
                    let new_path = dest.with_file_name(format!(
                        "{}.new",
                        dest.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    if new_path.exists() {
                        bail!("Refusing to overwrite existing {}", new_path.display());
                    }
                    copy_file(src, &new_path, entry.sudo)?;
                }
                // Preserve the old baseline if managed; a kept pre-existing file is unowned.
                return Ok(
                    if state.deployed.values().any(|paths| paths.contains(&key)) {
                        FileDeployment::KeptManaged
                    } else {
                        FileDeployment::KeptUnmanaged
                    },
                );
            }
        }
    }
    copy_file(src, dest, entry.sudo)?;
    state.hashes.insert(key, incoming_hash);
    Ok(FileDeployment::Written)
}

fn path_needs_sudo(component: &Component, path: &Path) -> bool {
    component.files.iter().any(|file| {
        file.sudo && destination_path(&file.dest).is_ok_and(|dest| path.starts_with(dest))
    })
}

/// Preserve modified files, external repositories, and files another component owns.
/// On a removal failure keep tracking and return the error so retry remains possible.
fn release_file(component: &Component, key: &str, state: &mut State) -> Result<()> {
    let shared = state
        .deployed
        .iter()
        .any(|(name, files)| name != &component.name && files.iter().any(|file| file == key));
    if !shared {
        let path = Path::new(key);
        let sudo = path_needs_sudo(component, path);
        if path
            .try_exists()
            .with_context(|| format!("Cannot inspect {}", path.display()))?
        {
            let unchanged = if path.is_file() {
                let contents = read_file_contents(path, sudo)?;
                state.hashes.get(key) == Some(&hash_contents(&contents))
            } else {
                false
            };
            if unchanged {
                remove_path(path, sudo)?;
            } else {
                println!("  Keeping modified or unverified path: {}", path.display());
            }
        }
        state.hashes.remove(key);
        state.sources.remove(key);
    }
    if let Some(files) = state.deployed.get_mut(&component.name) {
        files.retain(|file| file != key);
    }
    Ok(())
}

pub fn undeploy_component_files(component: &Component, state: &mut State) -> Result<()> {
    let files = state
        .deployed
        .get(&component.name)
        .cloned()
        .unwrap_or_default();
    for file in files {
        release_file(component, &file, state)?;
    }
    state.deployed.remove(&component.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture, write};

    fn component(dest: &Path) -> Component {
        toml::from_str(&format!(
            "name = 'test'\n[[files]]\nsrc = 'config'\ndest = '{}'\ncheck = false\n",
            dest.display()
        ))
        .unwrap()
    }

    #[test]
    fn cleanup_preserves_modified_files_and_removes_only_unchanged_owned_files() {
        let (root, config) = fixture();
        let dest = root.path().join("dest");
        write(config.dotfiles_dir().join("config/a"), "a");
        write(config.dotfiles_dir().join("config/b"), "b");
        let component = component(&dest);
        let mut state = State::default();
        redeploy_component_files(&component, &config, &mut state, Profile::default()).unwrap();
        write(dest.join("b"), "my changes");
        undeploy_component_files(&component, &mut state).unwrap();
        assert!(!dest.join("a").exists());
        assert_eq!(
            std::fs::read_to_string(dest.join("b")).unwrap(),
            "my changes"
        );
        assert!(!state.deployed.contains_key("test"));
        assert!(state.hashes.is_empty());
    }

    #[test]
    fn switching_device_removes_obsolete_override_files_and_restores_base() {
        let (root, config) = fixture();
        let dest = root.path().join("dest");
        write(config.dotfiles_dir().join("config/shared"), "base");
        write(
            config
                .repo_dir
                .join("devices/laptop/dotfiles/config/shared"),
            "laptop",
        );
        write(
            config
                .repo_dir
                .join("devices/laptop/dotfiles/config/laptop-only"),
            "battery",
        );
        let component = component(&dest);
        let mut state = State::default();
        redeploy_component_files(
            &component,
            &config,
            &mut state,
            Profile {
                environment: None,
                device: Some(crate::profile::Device::Laptop),
            },
        )
        .unwrap();
        redeploy_component_files(
            &component,
            &config,
            &mut state,
            Profile {
                environment: None,
                device: Some(crate::profile::Device::Desktop),
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("shared")).unwrap(),
            "base"
        );
        assert!(!dest.join("laptop-only").exists());
        assert_eq!(state.deployed["test"].len(), 1);
    }

    #[test]
    fn shared_ownership_survives_removing_one_component() {
        let (root, config) = fixture();
        let dest = root.path().join("dest");
        write(config.dotfiles_dir().join("config"), "value");
        let component = component(&dest);
        let mut state = State::default();
        redeploy_component_files(&component, &config, &mut state, Profile::default()).unwrap();
        state
            .deployed
            .insert("other".into(), vec![dest.to_string_lossy().into_owned()]);
        undeploy_component_files(&component, &mut state).unwrap();
        assert!(dest.exists());
        assert!(state.hashes.contains_key(dest.to_str().unwrap()));
    }

    #[test]
    fn failed_removal_preserves_tracking() {
        let (root, config) = fixture();
        let dest = root.path().join("dest");
        write(config.dotfiles_dir().join("config"), "value");
        let component = component(&dest);
        let mut state = State::default();
        redeploy_component_files(&component, &config, &mut state, Profile::default()).unwrap();
        // An unreadable path (a symlink loop) produces an error without needing root.
        std::fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink(&dest, &dest).unwrap();
        // release_file uses try_exists, so failure cannot look like a missing file.
        assert!(undeploy_component_files(&component, &mut state).is_err());
        assert_eq!(
            state.deployed["test"],
            vec![dest.to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn preflight_rejects_overlapping_component_destinations() {
        let (root, config) = fixture();
        write(config.dotfiles_dir().join("config"), "value");
        let first = component(&root.path().join("dest"));
        let mut second = first.clone();
        second.name = "other".into();
        let manifest = Manifest {
            environments: None,
            components: vec![first, second],
        };
        assert!(preflight(
            &manifest,
            &config,
            Profile::default(),
            &HashSet::from(["test", "other"])
        )
        .unwrap_err()
        .to_string()
        .contains("Conflicting destinations"));
    }
}

#[cfg(test)]
mod conflict_tests {
    use super::*;
    use crate::test_support::{fixture, write};

    #[test]
    fn keeping_a_preexisting_local_file_does_not_claim_ownership() {
        let (root, config) = fixture();
        let source = config.dotfiles_dir().join("config");
        let dest = root.path().join("dest");
        write(&source, "incoming");
        write(&dest, "mine");
        let entry: FileEntry =
            toml::from_str(&format!("src='config'\ndest='{}'", dest.display())).unwrap();
        for choice in [0, 2] {
            let mut state = State::default();
            assert_eq!(
                deploy_single_file_with_choice(&source, &dest, &entry, &mut state, || Ok(choice))
                    .unwrap(),
                FileDeployment::KeptUnmanaged
            );
            assert!(state.hashes.is_empty());
            assert_eq!(std::fs::read_to_string(&dest).unwrap(), "mine");
        }
        assert_eq!(
            std::fs::read_to_string(dest.with_file_name("dest.new")).unwrap(),
            "incoming"
        );
    }
}
