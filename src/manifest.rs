use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};

fn default_true() -> bool {
    true
}

/// Accept the current list syntax as well as the previous single-value form so
/// existing manifests continue to work after the scope model becomes plural.
fn deserialize_environments<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(environment) => vec![environment],
        OneOrMany::Many(environments) => environments,
    })
}

fn applies_to_environments(environments: &[String], active_environment: Option<&str>) -> bool {
    match active_environment {
        Some(active_environment) => {
            environments.is_empty()
                || environments
                    .iter()
                    .any(|environment| environment == active_environment)
        }
        None => environments.is_empty(),
    }
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    #[serde(default, alias = "shells")]
    pub environments: Option<EnvironmentConfig>,
    #[serde(default)]
    pub components: Vec<Component>,
}

#[derive(Debug, Deserialize)]
pub struct EnvironmentConfig {
    pub available: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Component {
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub ask: bool,
    #[serde(
        default,
        alias = "environment",
        alias = "shell",
        deserialize_with = "deserialize_environments"
    )]
    pub environments: Vec<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub post_install: Vec<HookEntry>,
    #[serde(default)]
    pub post_deploy: Vec<HookEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEntry {
    pub src: String,
    pub dest: String,
    #[serde(default)]
    pub sudo: bool,
    #[serde(default = "default_true")]
    pub check: bool,
    /// Optional: only deploy this file while one of these environments is active.
    #[serde(
        default,
        alias = "environment",
        alias = "shell",
        deserialize_with = "deserialize_environments"
    )]
    pub environments: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum HookEntry {
    Simple(String),
    Scoped {
        cmd: String,
        #[serde(
            default,
            alias = "environment",
            alias = "shell",
            deserialize_with = "deserialize_environments"
        )]
        environments: Vec<String>,
    },
}

impl HookEntry {
    pub fn cmd(&self) -> &str {
        match self {
            HookEntry::Simple(cmd) => cmd,
            HookEntry::Scoped { cmd, .. } => cmd,
        }
    }

    pub fn is_environment_scoped(&self) -> bool {
        match self {
            HookEntry::Simple(_) => false,
            HookEntry::Scoped { environments, .. } => !environments.is_empty(),
        }
    }

    pub fn applies_to_environment(&self, active_environment: Option<&str>) -> bool {
        match self {
            HookEntry::Simple(_) => true,
            HookEntry::Scoped { environments, .. } => {
                applies_to_environments(environments, active_environment)
            }
        }
    }
}

impl FileEntry {
    /// Whether this entry should be deployed for the currently active environment.
    ///
    /// Entries without environments are shared; scoped entries are deployed only
    /// when the active environment appears in their environment list.
    pub fn applies_to_environment(&self, active_environment: Option<&str>) -> bool {
        applies_to_environments(&self.environments, active_environment)
    }
}

impl Component {
    pub fn is_global(&self) -> bool {
        self.environments.is_empty()
    }

    pub fn applies_to_environment(&self, active_environment: Option<&str>) -> bool {
        applies_to_environments(&self.environments, active_environment)
    }

    pub fn environment_label(&self) -> Option<String> {
        (!self.environments.is_empty()).then(|| self.environments.join(", "))
    }
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read manifest: {}", path.display()))?;
        let manifest: Manifest =
            toml::from_str(&content).with_context(|| "Failed to parse manifest.toml")?;
        Ok(manifest)
    }

    pub fn available_environments(&self) -> Vec<&str> {
        self.environments
            .as_ref()
            .map(|s| s.available.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Returns components visible for the given active environment.
    /// Global components are always visible. Environment-specific components are
    /// visible when their environment list includes the active environment.
    pub fn components_for_environment(&self, env: Option<&str>) -> Vec<&Component> {
        self.components
            .iter()
            .filter(|component| component.applies_to_environment(env))
            .collect()
    }

    /// Returns only non-global components whose environment list contains `env`.
    pub fn components_only_for_environment(&self, env: &str) -> Vec<&Component> {
        self.components
            .iter()
            .filter(|component| !component.is_global() && component.applies_to_environment(Some(env)))
            .collect()
    }

    pub fn find_component(&self, name: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::{FileEntry, Manifest};

    fn file_entry(environments: &[&str]) -> FileEntry {
        FileEntry {
            src: "source".into(),
            dest: "destination".into(),
            sudo: false,
            check: true,
            environments: environments.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn unscoped_files_apply_to_every_environment() {
        let entry = file_entry(&[]);

        assert!(entry.applies_to_environment(None));
        assert!(entry.applies_to_environment(Some("caelestia")));
    }

    #[test]
    fn scoped_files_apply_to_each_listed_environment() {
        let entry = file_entry(&["caelestia", "noctalia"]);

        assert!(entry.applies_to_environment(Some("caelestia")));
        assert!(entry.applies_to_environment(Some("noctalia")));
        assert!(!entry.applies_to_environment(Some("kde")));
        assert!(!entry.applies_to_environment(None));
    }

    #[test]
    fn components_and_hooks_accept_environment_lists() {
        let manifest: Manifest = toml::from_str(
            r#"
            [[components]]
            name = "shared-environment-component"
            environments = ["caelestia", "noctalia"]
            post_deploy = [{ cmd = "reload", environments = ["caelestia", "noctalia"] }]
            "#,
        )
        .unwrap();

        let component = &manifest.components[0];
        assert!(component.applies_to_environment(Some("caelestia")));
        assert!(component.applies_to_environment(Some("noctalia")));
        assert!(!component.applies_to_environment(Some("kde")));
        assert!(component.post_deploy[0].applies_to_environment(Some("noctalia")));
    }

    #[test]
    fn legacy_single_environment_is_still_accepted() {
        let manifest: Manifest = toml::from_str(
            r#"
            [[components]]
            name = "legacy-component"
            environment = "caelestia"
            "#,
        )
        .unwrap();

        assert_eq!(manifest.components[0].environments, ["caelestia"]);
    }

    #[test]
    fn component_ask_defaults_to_false() {
        let manifest: Manifest = toml::from_str(
            r#"
            [[components]]
            name = "normal"

            [[components]]
            name = "optional"
            ask = true
            "#,
        )
        .unwrap();

        assert!(!manifest.components[0].ask);
        assert!(manifest.components[1].ask);
    }
}
