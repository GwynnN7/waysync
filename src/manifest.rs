use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::profile::{Device, Profile};

use anyhow::{bail, Context, Result};
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

#[derive(Deserialize)]
struct ManifestDocument {
    #[serde(default)]
    include: Vec<String>,
    #[serde(flatten)]
    manifest: Manifest,
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
    pub devices: Vec<Device>,
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
    #[serde(default)]
    pub devices: Vec<Device>,
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
        #[serde(default)]
        devices: Vec<Device>,
    },
}

impl HookEntry {
    pub fn cmd(&self) -> &str {
        match self {
            HookEntry::Simple(cmd) => cmd,
            HookEntry::Scoped { cmd, .. } => cmd,
        }
    }

    pub fn is_scoped(&self) -> bool {
        matches!(self, HookEntry::Scoped { environments, devices, .. } if !environments.is_empty() || !devices.is_empty())
    }

    pub fn applies_to(&self, profile: Profile<'_>) -> bool {
        match self {
            HookEntry::Simple(_) => true,
            HookEntry::Scoped {
                environments,
                devices,
                ..
            } => {
                applies_to_environments(environments, profile.environment)
                    && (devices.is_empty()
                        || profile
                            .device
                            .is_some_and(|device| devices.contains(&device)))
            }
        }
    }
}

impl FileEntry {
    pub fn applies_to(&self, profile: Profile<'_>) -> bool {
        applies_to_environments(&self.environments, profile.environment)
            && (self.devices.is_empty()
                || profile
                    .device
                    .is_some_and(|device| self.devices.contains(&device)))
    }
}

impl Component {
    pub fn applies_to(&self, profile: Profile<'_>) -> bool {
        applies_to_environments(&self.environments, profile.environment)
            && (self.devices.is_empty()
                || profile
                    .device
                    .is_some_and(|device| self.devices.contains(&device)))
    }

    pub fn environment_label(&self) -> Option<String> {
        (!self.environments.is_empty()).then(|| self.environments.join(", "))
    }
}

impl Manifest {
    /// Literal fragment paths are read in the specified order, relative to the
    /// root manifest. No globs, nested includes, symlinks, or checkout escapes.
    pub fn documents(path: &Path) -> Result<Vec<(PathBuf, String)>> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read manifest: {}", path.display()))?;
        let root: ManifestDocument = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        let directory = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()?;
        let mut seen = HashSet::from([path.canonicalize()?]);
        let mut documents = vec![(path.to_path_buf(), content)];
        for include in root.include {
            if !safe_relative_path(&include) {
                bail!("Manifest include must stay inside the checkout: {include}");
            }
            let mut included = directory.clone();
            for part in Path::new(&include).components() {
                included.push(part);
                if std::fs::symlink_metadata(&included)
                    .with_context(|| format!("Cannot read included manifest {include}"))?
                    .file_type()
                    .is_symlink()
                {
                    bail!("Manifest includes cannot follow symlinks: {include}");
                }
            }
            let included = included.canonicalize()?;
            if !included.starts_with(&directory) || !seen.insert(included.clone()) {
                bail!("Duplicate or escaping manifest include: {include}");
            }
            let content = std::fs::read_to_string(&included)?;
            let fragment: ManifestDocument = toml::from_str(&content)
                .with_context(|| format!("Failed to parse fragment {include}"))?;
            if !fragment.include.is_empty() || fragment.manifest.environments.is_some() {
                bail!("Fragment '{include}' may define components only; environments and includes belong in the root manifest");
            }
            documents.push((included, content));
        }
        Ok(documents)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut documents = Self::documents(path)?.into_iter();
        let (_, content) = documents.next().context("Missing root manifest")?;
        let mut root: ManifestDocument = toml::from_str(&content)?;
        for (path, content) in documents {
            let fragment: ManifestDocument = toml::from_str(&content)
                .with_context(|| format!("Failed to parse {}", path.display()))?;
            root.manifest
                .components
                .extend(fragment.manifest.components);
        }
        root.manifest.validate()?;
        Ok(root.manifest)
    }

    pub fn available_environments(&self) -> Vec<&str> {
        self.environments
            .as_ref()
            .map(|s| s.available.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    pub fn components_for_profile(&self, profile: Profile<'_>) -> Vec<&Component> {
        self.components
            .iter()
            .filter(|component| component.applies_to(profile))
            .collect()
    }

    pub fn validate(&self) -> Result<()> {
        let mut environments = HashSet::new();
        for environment in self.available_environments() {
            if !safe_relative_path(environment)
                || environment.contains('/')
                || !environments.insert(environment)
            {
                bail!("Invalid or duplicate environment name: {environment}");
            }
        }
        let mut names = HashSet::new();
        for component in &self.components {
            if component.name.trim().is_empty() || !names.insert(&component.name) {
                bail!("Empty or duplicate component name: {}", component.name);
            }
            let scopes = std::iter::once(&component.environments)
                .chain(component.files.iter().map(|file| &file.environments))
                .chain(
                    component
                        .post_install
                        .iter()
                        .chain(&component.post_deploy)
                        .filter_map(|hook| {
                            if let HookEntry::Scoped { environments, .. } = hook {
                                Some(environments)
                            } else {
                                None
                            }
                        }),
                );
            for scope in scopes {
                for environment in scope {
                    if !environments.contains(environment.as_str()) {
                        bail!(
                            "Component '{}': undeclared environment '{environment}'",
                            component.name
                        );
                    }
                }
            }
            for file in &component.files {
                if let Some(repo) = file.src.strip_prefix("repo:") {
                    if repo.split('/').count() != 2 || !safe_relative_path(repo) {
                        bail!("Invalid repository source: {}", file.src);
                    }
                    if file.sudo {
                        bail!("repo: sources do not support sudo: {}", file.src);
                    }
                } else if !safe_relative_path(&file.src) {
                    bail!("Source must be a relative path without '..': {}", file.src);
                }
                crate::utils::destination_path(&file.dest)?;
            }
            for package in &component.packages {
                let name = package
                    .strip_prefix("local:")
                    .or_else(|| package.strip_prefix("aur:"))
                    .unwrap_or(package);
                if name.is_empty() || name.starts_with('-') || !safe_relative_path(name) {
                    bail!("Invalid package specification: {package}");
                }
            }
        }
        Ok(())
    }

    pub fn find_component(&self, name: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.name == name)
    }
}

fn safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::{FileEntry, Manifest};
    use crate::profile::Profile;

    fn file_entry(environments: &[&str]) -> FileEntry {
        FileEntry {
            src: "source".into(),
            dest: "destination".into(),
            sudo: false,
            check: true,
            devices: Vec::new(),
            environments: environments.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn unscoped_files_apply_to_every_environment() {
        let entry = file_entry(&[]);

        assert!(entry.applies_to(Profile::default()));
        assert!(entry.applies_to(Profile {
            environment: Some("caelestia"),
            device: None
        }));
    }

    #[test]
    fn scoped_files_apply_to_each_listed_environment() {
        let entry = file_entry(&["caelestia", "noctalia"]);

        assert!(entry.applies_to(Profile {
            environment: Some("caelestia"),
            device: None
        }));
        assert!(entry.applies_to(Profile {
            environment: Some("noctalia"),
            device: None
        }));
        assert!(!entry.applies_to(Profile {
            environment: Some("kde"),
            device: None
        }));
        assert!(!entry.applies_to(Profile::default()));
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
        assert!(component.applies_to(Profile {
            environment: Some("caelestia"),
            device: None
        }));
        assert!(component.applies_to(Profile {
            environment: Some("noctalia"),
            device: None
        }));
        assert!(!component.applies_to(Profile {
            environment: Some("kde"),
            device: None
        }));
        assert!(component.post_deploy[0].applies_to(Profile {
            environment: Some("noctalia"),
            device: None
        }));
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

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn both_scope_axes_must_match_for_components_files_and_hooks() {
        let manifest: Manifest = toml::from_str(
            r#"
            [environments]
            available = ["shell", "other"]
            [[components]]
            name = "hardware"
            environments = ["shell"]
            devices = ["laptop"]
            post_deploy = [{ cmd = "reload", environments = ["shell"], devices = ["laptop"] }]
            [[components.files]]
            src = "config"
            dest = "/tmp/waysync-example-config"
            environments = ["shell"]
            devices = ["laptop"]
        "#,
        )
        .unwrap();
        manifest.validate().unwrap();
        for environment in [None, Some("shell"), Some("other")] {
            for device in [None, Some(Device::Desktop), Some(Device::Laptop)] {
                let profile = Profile {
                    environment,
                    device,
                };
                let expected = environment == Some("shell") && device == Some(Device::Laptop);
                assert_eq!(manifest.components[0].applies_to(profile), expected);
                assert_eq!(
                    manifest.components[0].files[0].applies_to(profile),
                    expected
                );
                assert_eq!(
                    manifest.components[0].post_deploy[0].applies_to(profile),
                    expected
                );
            }
        }
    }

    #[test]
    fn invalid_manifest_names_and_sources_are_rejected() {
        for text in [
            "[[components]]\nname='same'\n[[components]]\nname='same'",
            "[environments]\navailable=['../escape']",
            "[[components]]\nname='a'\nenvironment='typo'",
            "[[components]]\nname='a'\n[[components.files]]\nsrc='../escape'\ndest='/tmp/waysync-config'",
            "[[components]]\nname='a'\n[[components.files]]\nsrc='config'\ndest='$HOME'",
            "[[components]]\nname='a'\npackages=['local:../../escape']",
        ] {
            let manifest: Manifest = toml::from_str(text).unwrap();
            assert!(manifest.validate().is_err(), "Should reject: {text}");
        }
    }
}

#[cfg(test)]
mod include_tests {
    use super::*;
    use crate::test_support::{fixture, write};

    #[test]
    fn literal_fragments_merge_in_order_and_validate_scopes_together() {
        let (_root, config) = fixture();
        write(config.manifest_path(), "include=['components/desktop.toml','components/tools.toml']\n[environments]\navailable=['shell']\n[[components]]\nname='root'\n");
        write(
            config.repo_dir.join("components/desktop.toml"),
            "[[components]]\nname='desktop'\nenvironment='shell'\n",
        );
        write(
            config.repo_dir.join("components/tools.toml"),
            "[[components]]\nname='tools'\n",
        );
        let manifest = Manifest::load(&config.manifest_path()).unwrap();
        assert_eq!(
            manifest
                .components
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "desktop", "tools"]
        );
    }

    #[test]
    fn includes_reject_escape_symlinks_nested_includes_and_duplicate_components() {
        let (root, config) = fixture();
        write(root.path().join("outside.toml"), "");
        write(config.manifest_path(), "include=['../outside.toml']\n");
        assert!(Manifest::load(&config.manifest_path()).is_err());
        std::os::unix::fs::symlink(
            root.path().join("outside.toml"),
            config.repo_dir.join("link.toml"),
        )
        .unwrap();
        write(config.manifest_path(), "include=['link.toml']\n");
        assert!(Manifest::load(&config.manifest_path()).is_err());
        write(
            config.manifest_path(),
            "include=['fragment.toml']\n[[components]]\nname='same'\n",
        );
        write(
            config.repo_dir.join("fragment.toml"),
            "include=['again.toml']\n",
        );
        assert!(Manifest::load(&config.manifest_path()).is_err());
        write(
            config.repo_dir.join("fragment.toml"),
            "[[components]]\nname='same'\n",
        );
        assert!(Manifest::load(&config.manifest_path()).is_err());
    }
}
