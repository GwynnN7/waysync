use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Result};

use crate::config::AppConfig;
use crate::manifest::Manifest;
use crate::profile::{Device, Profile};
use crate::state::State;

/// Find fields Serde would otherwise silently ignore, especially misplaced hooks.
pub fn unknown_fields(content: &str) -> Result<Vec<String>> {
    let document: toml::Value = content.parse()?;
    let mut warnings = Vec::new();
    fn inspect(value: &toml::Value, known: &[&str], location: &str, warnings: &mut Vec<String>) {
        if let Some(table) = value.as_table() {
            for key in table.keys().filter(|key| !known.contains(&key.as_str())) {
                warnings.push(format!(
                    "{location}.{key} is ignored.{}",
                    if key == "post_install" || key == "post_deploy" {
                        " Move hooks before the first [[components.files]] table."
                    } else {
                        " Check the field name."
                    }
                ));
            }
        }
    }
    inspect(
        &document,
        &["environments", "shells", "components", "include"],
        "manifest",
        &mut warnings,
    );
    for key in ["environments", "shells"] {
        if let Some(config) = document.get(key) {
            inspect(config, &["available"], key, &mut warnings);
        }
    }
    if let Some(components) = document
        .get("components")
        .and_then(|value| value.as_array())
    {
        for (index, component) in components.iter().enumerate() {
            let location = format!(
                "components[{}] ({})",
                index + 1,
                component
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unnamed")
            );
            inspect(
                component,
                &[
                    "name",
                    "category",
                    "default",
                    "ask",
                    "environments",
                    "environment",
                    "shell",
                    "devices",
                    "packages",
                    "files",
                    "post_install",
                    "post_deploy",
                ],
                &location,
                &mut warnings,
            );
            if let Some(files) = component.get("files").and_then(|v| v.as_array()) {
                for (index, file) in files.iter().enumerate() {
                    inspect(
                        file,
                        &[
                            "src",
                            "dest",
                            "sudo",
                            "check",
                            "environments",
                            "environment",
                            "shell",
                            "devices",
                        ],
                        &format!("{location}.files[{}]", index + 1),
                        &mut warnings,
                    );
                }
            }
            for hook_type in ["post_install", "post_deploy"] {
                if let Some(hooks) = component.get(hook_type).and_then(|v| v.as_array()) {
                    for hook in hooks {
                        inspect(
                            hook,
                            &["cmd", "environments", "environment", "shell", "devices"],
                            &format!("{location}.{hook_type}"),
                            &mut warnings,
                        );
                    }
                }
            }
        }
    }
    Ok(warnings)
}

pub fn check(config: &AppConfig, checkout: Option<&Path>, all: bool) -> Result<()> {
    let override_config;
    let config = if let Some(checkout) = checkout {
        override_config = AppConfig {
            repo_dir: checkout.canonicalize()?,
            state_file: config.state_file.clone(),
            state_dir: config.state_dir.clone(),
            data_dir: config.data_dir.clone(),
        };
        &override_config
    } else {
        config
    };
    let manifest = Manifest::load(&config.manifest_path())?;
    let mut warnings = Vec::new();
    for (path, content) in Manifest::documents(&config.manifest_path())? {
        warnings.extend(
            unknown_fields(&content)?
                .into_iter()
                .map(|warning| format!("{}: {warning}", path.display())),
        );
    }
    for warning in &warnings {
        println!("Warning: {warning}");
    }
    let state = if checkout.is_none() {
        State::load(&config.state_file)?
    } else {
        State::default()
    };
    let mut profiles = vec![state.profile()];
    if all {
        profiles.clear();
        for environment in
            std::iter::once(None).chain(manifest.available_environments().into_iter().map(Some))
        {
            for device in [None, Some(Device::Desktop), Some(Device::Laptop)] {
                profiles.push(Profile {
                    environment,
                    device,
                });
            }
        }
    }
    let mut failures = 0;
    for profile in profiles {
        println!(
            "Checking environment={}, device={}",
            profile.environment.unwrap_or("none"),
            profile
                .device
                .map(|device| device.to_string())
                .unwrap_or_else(|| "none".into())
        );
        let components = manifest.components_for_profile(profile);
        // Check all visible components, including optional ones, without executing recipes.
        let names: HashSet<_> = components.iter().map(|c| c.name.as_str()).collect();
        for component in &components {
            if let Err(error) = crate::deploy::preflight(
                &manifest,
                config,
                profile,
                &HashSet::from([component.name.as_str()]),
            ) {
                println!("  Error: {error:#}");
                failures += 1;
            }
        }
        // Only compare components that resolved successfully to avoid hiding collisions.
        if let Err(error) = crate::deploy::preflight(&manifest, config, profile, &names) {
            if error.to_string().starts_with("Conflicting destinations") {
                println!("  Error: {error:#}");
                failures += 1;
            }
        }
    }
    println!(
        "{} components; {} ignored field(s); {} validation error(s).",
        manifest.components.len(),
        warnings.len(),
        failures
    );
    if failures > 0 || !warnings.is_empty() {
        bail!("Configuration check found issues; see diagnostics above");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misplaced_hooks_and_typos_produce_actionable_diagnostics() {
        let warnings = unknown_fields(
            r#"
            [[components]]
            name = "plugins"
            defualt = true
            [[components.files]]
            src = "repo:owner/project"
            dest = "~/project"
            post_deploy = ["reload"]
        "#,
        )
        .unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|warning| warning.contains("defualt")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("Move hooks before")));
    }
}
