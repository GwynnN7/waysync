use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::profile::{Device, Profile};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default)]
    pub repo_url: Option<String>,
    /// Optional authoring checkout used for capturing deployed edits.
    #[serde(default)]
    pub working_repo: Option<std::path::PathBuf>,
    #[serde(default, alias = "active_shell")]
    pub active_environment: Option<String>,
    #[serde(default)]
    pub active_device: Option<Device>,
    #[serde(default)]
    /// Remembered desired choices, including currently inactive profiles.
    pub components: HashMap<String, bool>,
    /// Tracked component installation state; sync audits actual package presence.
    #[serde(default)]
    pub installed: Option<HashSet<String>>,
    #[serde(default)]
    pub pending_install: HashSet<String>,
    #[serde(default)]
    pub hashes: HashMap<String, String>,
    /// Original repository-relative source for each deployed local file.
    #[serde(default)]
    pub sources: HashMap<String, String>,
    /// Tracks which files were deployed per component for clean undeploy.
    #[serde(default)]
    pub deployed: HashMap<String, Vec<String>>,
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read state: {}", path.display()))?;
            let state: State =
                toml::from_str(&content).with_context(|| "Failed to parse state file")?;
            Ok(state)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize state")?;
        use std::io::Write;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(content.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .with_context(|| format!("Failed to replace state: {}", path.display()))?;
        Ok(())
    }

    pub fn migrate(&mut self, manifest: &crate::manifest::Manifest) {
        if self.installed.is_none() {
            self.installed = Some(
                manifest
                    .components_for_profile(self.profile())
                    .iter()
                    .filter(|component| self.is_enabled(&component.name))
                    .map(|component| component.name.clone())
                    .chain(self.deployed.keys().cloned())
                    .collect(),
            );
        }
    }

    pub fn is_installed(&self, name: &str) -> bool {
        self.installed
            .as_ref()
            .is_some_and(|names| names.contains(name))
            || self.deployed.contains_key(name)
    }

    pub fn mark_installed(&mut self, name: &str, installed: bool) {
        let names = self.installed.get_or_insert_with(HashSet::new);
        if installed {
            names.insert(name.to_string());
        } else {
            names.remove(name);
            self.pending_install.remove(name);
        }
    }

    pub fn profile(&self) -> Profile<'_> {
        Profile {
            environment: self.active_environment.as_deref(),
            device: self.active_device,
        }
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.components.get(name).copied().unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture, write};

    #[test]
    fn legacy_state_migrates_and_atomic_save_roundtrips_profiles_and_ownership() {
        let (_root, config) = fixture();
        write(
            &config.state_file,
            "active_shell = 'shell'\n[components]\nshared = true\nother = true\n",
        );
        let manifest = toml::from_str("[environments]\navailable=['shell', 'other']\n[[components]]\nname='shared'\n[[components]]\nname='other'\nenvironment='other'").unwrap();
        let mut state = State::load(&config.state_file).unwrap();
        state.migrate(&manifest);
        assert!(state.is_installed("shared"));
        assert!(!state.is_installed("other"));
        assert!(state.is_enabled("other"));
        state.active_device = Some(Device::Laptop);
        state.save(&config.state_file).unwrap();
        let saved = State::load(&config.state_file).unwrap();
        assert_eq!(saved.active_environment.as_deref(), Some("shell"));
        assert_eq!(saved.active_device, Some(Device::Laptop));
        assert_eq!(saved.installed, state.installed);
    }
}
