use std::fmt;
use std::path::PathBuf;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;

/// An independent hardware axis; an unset device preserves existing behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Desktop,
    Laptop,
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Desktop => "desktop",
            Self::Laptop => "laptop",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Profile<'a> {
    pub environment: Option<&'a str>,
    pub device: Option<Device>,
}

impl Profile<'_> {
    /// Ordered from lowest to highest priority. Keep legacy environment paths.
    pub fn source_roots(self, config: &AppConfig) -> Vec<PathBuf> {
        let mut roots = vec![config.dotfiles_dir()];
        if let Some(environment) = self.environment {
            roots.push(config.override_dir(environment));
        }
        if let Some(device) = self.device {
            let device_root = config.repo_dir.join("devices").join(device.to_string());
            roots.push(device_root.join("dotfiles"));
            if let Some(environment) = self.environment {
                roots.push(device_root.join("overrides").join(environment));
            }
        }
        roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture, write};
    use crate::utils::{collect_directory_files, resolve_file_source, FileSource};

    #[test]
    fn all_four_layers_merge_with_device_environment_highest() {
        let (_root, config) = fixture();
        let profile = Profile {
            environment: Some("shell"),
            device: Some(Device::Laptop),
        };
        for (index, root) in profile.source_roots(&config).into_iter().enumerate() {
            write(root.join("config/shared"), &index.to_string());
            write(root.join(format!("config/layer{index}")), "unique");
        }
        let files = collect_directory_files("config", &config, profile).unwrap();
        assert_eq!(files.len(), 5);
        let shared = files
            .iter()
            .find(|(_, relative)| relative == Path::new("shared"))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&shared.0).unwrap(), "3");
        match resolve_file_source("config/shared", &config, profile) {
            FileSource::Local(path) => assert_eq!(std::fs::read_to_string(path).unwrap(), "3"),
            _ => panic!("Expected local source"),
        }
        let without_device = Profile {
            environment: Some("shell"),
            device: None,
        };
        let files = collect_directory_files("config", &config, without_device).unwrap();
        assert_eq!(files.len(), 3);
        let shared = files
            .iter()
            .find(|(_, relative)| relative == Path::new("shared"))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&shared.0).unwrap(), "1");
    }

    use std::path::Path;
}
