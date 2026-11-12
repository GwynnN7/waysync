use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::config::AppConfig;
use crate::profile::Profile;

/// Resolved source for a file entry.
pub enum FileSource {
    /// A local path (file or directory) in the dotfiles/overrides tree.
    Local(PathBuf),
    /// A git repository URL to clone.
    Repo(String),
}

/// Expand environment variables in a string.
/// Handles $VAR, ${VAR}, and ${VAR:-default} patterns.
pub fn expand_env(s: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let get_var = |name: &str| -> String {
        std::env::var(name).unwrap_or_else(|_| match name {
            "XDG_CONFIG_HOME" => format!("{}/.config", home),
            "XDG_DATA_HOME" => format!("{}/.local/share", home),
            "XDG_STATE_HOME" => format!("{}/.local/state", home),
            "XDG_CACHE_HOME" => format!("{}/.cache", home),
            "XDG_BIN_HOME" => format!("{}/.local/bin", home),
            "XDG_CONFIG_SERVICE" => format!(
                "{}/systemd/user",
                std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home))
            ),
            "HOME" => home.clone(),
            _ => String::new(),
        })
    };

    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if chars.peek() == Some(&'{') {
                chars.next(); // consume '{'
                let mut var_expr = String::new();
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    var_expr.push(c);
                }
                if let Some(idx) = var_expr.find(":-") {
                    let var_name = &var_expr[..idx];
                    let default = &var_expr[idx + 2..];
                    let val = std::env::var(var_name)
                        .ok()
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| expand_env(default));
                    result.push_str(&val);
                } else {
                    result.push_str(&get_var(&var_expr));
                }
            } else {
                // Read var name: alphanumeric + underscore
                let mut var_name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' {
                        var_name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if var_name.is_empty() {
                    result.push('$');
                } else {
                    result.push_str(&get_var(&var_name));
                }
            }
        } else {
            result.push(c);
        }
    }

    if result == "~" {
        home
    } else if let Some(rest) = result.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        result
    }
}

/// Reject unresolved variables and broad destinations before copying or removing.
pub fn destination_path(value: &str) -> Result<PathBuf> {
    // Unknown variables must not silently collapse to a different absolute path.
    let expanded = expand_env(value);
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            continue;
        }
        let expression = if chars.peek() == Some(&'{') {
            chars.next();
            let mut expression = String::new();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == '}' {
                    closed = true;
                    break;
                }
                expression.push(c);
            }
            if !closed {
                bail!("Unclosed variable in destination: {value}");
            }
            expression
        } else {
            let mut name = String::new();
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                name.push(chars.next().unwrap());
            }
            name
        };
        if !expression.contains(":-")
            && (expression.is_empty() || expand_env(&format!("${{{expression}}}")).is_empty())
        {
            bail!("Unset or empty variable '{expression}' in destination: {value}");
        }
    }
    let path = PathBuf::from(&expanded);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("Destination must expand to an absolute path without '..': {value}");
    }
    let protected = [
        "/",
        "/etc",
        "/usr",
        "/usr/bin",
        "/usr/local",
        "/usr/local/bin",
        "/usr/lib",
        "/usr/share",
        "/var",
        "/tmp",
        "$HOME",
        "$XDG_CONFIG_HOME",
        "$XDG_DATA_HOME",
        "$XDG_STATE_HOME",
        "$XDG_CACHE_HOME",
        "$XDG_BIN_HOME",
        "$XDG_CONFIG_SERVICE",
    ];
    if protected
        .iter()
        .any(|root| path == Path::new(&expand_env(root)))
    {
        bail!("Destination is a protected root: {}", path.display());
    }
    Ok(path)
}

/// Compute SHA256 hash of a byte slice.
pub fn hash_contents(contents: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(contents);
    hex::encode(hasher.finalize())
}

/// Resolve a file source string to a FileSource.
/// Handles "repo:user/repo" -> Repo, otherwise Local with override resolution.
pub fn resolve_file_source(src: &str, config: &AppConfig, profile: Profile<'_>) -> FileSource {
    if let Some(repo) = src.strip_prefix("repo:") {
        FileSource::Repo(format!("https://github.com/{}", repo))
    } else {
        FileSource::Local(resolve_local_path(src, config, profile))
    }
}

/// Resolve a local dotfile path with environment override priority.
/// Checks overrides/{environment}/{src} first, then dotfiles/{src}.
fn resolve_local_path(src: &str, config: &AppConfig, profile: Profile<'_>) -> PathBuf {
    profile
        .source_roots(config)
        .into_iter()
        .rev()
        .map(|root| root.join(src))
        .find(|path| path.exists())
        .unwrap_or_else(|| config.dotfiles_dir().join(src))
}

/// Merge each source layer by relative path; propagate traversal errors.
pub fn collect_directory_files(
    src: &str,
    config: &AppConfig,
    profile: Profile<'_>,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut files: HashMap<PathBuf, PathBuf> = HashMap::new();
    for root in profile.source_roots(config) {
        let directory = root.join(src);
        if !directory.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&directory) {
            let entry = entry.with_context(|| format!("Cannot read {}", directory.display()))?;
            if entry.file_type().is_symlink() {
                bail!(
                    "Symlink sources are unsupported; use a regular file: {}",
                    entry.path().display()
                );
            }
            if entry.file_type().is_file() {
                let relative = entry.path().strip_prefix(&directory)?;
                files.insert(relative.to_path_buf(), entry.path().to_path_buf());
            }
        }
    }
    let mut result: Vec<_> = files.into_iter().map(|(rel, src)| (src, rel)).collect();
    result.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(result)
}

/// Copy beside the destination and atomically replace it, preserving executable bits.
pub fn copy_file(src: &Path, dest: &Path, sudo: bool) -> Result<()> {
    let parent = dest.parent().context("Destination has no parent")?;
    if sudo {
        run_cmd("sudo", &["mkdir", "-p", "--", &parent.to_string_lossy()])?;
        // Allocate securely rather than using a predictable PID-based filename.
        let output = Command::new("sudo")
            .args(["mktemp", "--tmpdir"])
            .arg(parent)
            .arg(".waysync-XXXXXXXXXX")
            .output()
            .context("Failed to allocate privileged temporary file")?;
        if !output.status.success() {
            bail!("sudo mktemp failed in {}", parent.display());
        }
        let temp = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        let result = (|| -> Result<()> {
            run_cmd(
                "sudo",
                &[
                    "cp",
                    "--preserve=mode",
                    "--",
                    &src.to_string_lossy(),
                    &temp.to_string_lossy(),
                ],
            )?;
            if dest.starts_with("/etc/sudoers.d") {
                run_cmd("sudo", &["chmod", "0440", "--", &temp.to_string_lossy()])?;
                run_cmd("sudo", &["visudo", "-cf", &temp.to_string_lossy()])?;
            }
            run_cmd(
                "sudo",
                &[
                    "mv",
                    "-fT",
                    "--",
                    &temp.to_string_lossy(),
                    &dest.to_string_lossy(),
                ],
            )
        })();
        if result.is_err() {
            let _ = Command::new("sudo")
                .args(["rm", "-f", "--"])
                .arg(&temp)
                .status();
        }
        result
    } else {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        let temp = tempfile::Builder::new()
            .prefix(".waysync-")
            .tempfile_in(parent)?;
        fs::copy(src, temp.path())
            .with_context(|| format!("Failed to copy {} -> {}", src.display(), dest.display()))?;
        temp.as_file().sync_all()?;
        temp.persist(dest)
            .with_context(|| format!("Failed to replace {}", dest.display()))?;
        Ok(())
    }
}

/// Remove a file or directory, optionally with sudo.
pub fn remove_path(path: &Path, sudo: bool) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if sudo {
        run_cmd("sudo", &["rm", "-f", "--", &path.to_string_lossy()])?;
    } else if path.is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove directory: {}", path.display()))?;
    } else {
        fs::remove_file(path)
            .with_context(|| format!("Failed to remove file: {}", path.display()))?;
    }
    Ok(())
}

/// Read file contents, using sudo if needed.
pub fn read_file_contents(path: &Path, sudo: bool) -> Result<Vec<u8>> {
    if sudo {
        let output = Command::new("sudo")
            .args(["cat"])
            .arg(path)
            .output()
            .with_context(|| format!("Failed to read {} with sudo", path.display()))?;
        if !output.status.success() {
            bail!("sudo cat failed for {}", path.display());
        }
        Ok(output.stdout)
    } else {
        Ok(fs::read(path).with_context(|| format!("Failed to read: {}", path.display()))?)
    }
}

/// Run a command and check its exit status.
fn run_cmd(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("Failed to run: {} {}", program, args.join(" ")))?;
    if !status.success() {
        bail!(
            "{} {} failed with exit code {:?}",
            program,
            args.join(" "),
            status.code()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_config() -> AppConfig {
        let unique = format!(
            "waysync-utils-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before Unix epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let state_dir = root.join("state");
        let data_dir = root.join("data");

        AppConfig {
            state_file: state_dir.join("state.toml"),
            repo_dir: data_dir.join("repo"),
            state_dir,
            data_dir,
        }
    }

    fn local_path(source: FileSource) -> PathBuf {
        match source {
            FileSource::Local(path) => path,
            FileSource::Repo(_) => panic!("expected a local file source"),
        }
    }

    #[test]
    fn environment_override_replaces_a_single_base_file() {
        let config = test_config();
        let base = config.dotfiles_dir().join("theme.toml");
        let environment_override = config.override_dir("caelestia").join("theme.toml");
        std::fs::create_dir_all(base.parent().expect("base parent")).unwrap();
        std::fs::create_dir_all(environment_override.parent().expect("override parent")).unwrap();
        std::fs::write(&base, "base").unwrap();
        std::fs::write(&environment_override, "caelestia").unwrap();

        assert_eq!(
            local_path(resolve_file_source(
                "theme.toml",
                &config,
                Profile::default()
            )),
            base
        );
        assert_eq!(
            local_path(resolve_file_source(
                "theme.toml",
                &config,
                Profile {
                    environment: Some("caelestia"),
                    device: None
                }
            )),
            environment_override
        );
        assert_eq!(
            local_path(resolve_file_source(
                "theme.toml",
                &config,
                Profile {
                    environment: Some("noctalia"),
                    device: None
                }
            )),
            config.dotfiles_dir().join("theme.toml")
        );

        std::fs::remove_dir_all(config.data_dir.parent().expect("test root")).unwrap();
    }

    #[test]
    fn environment_override_merges_directories_by_relative_path() {
        let config = test_config();
        let base = config.dotfiles_dir().join("hypr");
        let environment_override = config.override_dir("caelestia").join("hypr");
        std::fs::create_dir_all(base.join("nested")).unwrap();
        std::fs::create_dir_all(environment_override.join("nested")).unwrap();
        std::fs::write(base.join("base-only.conf"), "base-only").unwrap();
        std::fs::write(base.join("replace.conf"), "base").unwrap();
        std::fs::write(base.join("nested/base-only.conf"), "nested-base").unwrap();
        std::fs::write(environment_override.join("replace.conf"), "caelestia").unwrap();
        std::fs::write(
            environment_override.join("nested/override-only.conf"),
            "nested-override",
        )
        .unwrap();

        let files: BTreeMap<_, _> = collect_directory_files(
            "hypr",
            &config,
            Profile {
                environment: Some("caelestia"),
                device: None,
            },
        )
        .unwrap()
        .into_iter()
        .map(|(source, relative)| (relative, std::fs::read_to_string(source).unwrap()))
        .collect();

        assert_eq!(
            files,
            BTreeMap::from([
                (PathBuf::from("base-only.conf"), "base-only".to_string()),
                (PathBuf::from("replace.conf"), "caelestia".to_string()),
                (
                    PathBuf::from("nested/base-only.conf"),
                    "nested-base".to_string()
                ),
                (
                    PathBuf::from("nested/override-only.conf"),
                    "nested-override".to_string()
                ),
            ])
        );

        std::fs::remove_dir_all(config.data_dir.parent().expect("test root")).unwrap();
    }

    #[test]
    fn copy_file_replaces_an_existing_file() {
        let root = std::env::temp_dir().join(format!(
            "waysync-copy-file-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::write(&source, "new content").unwrap();
        std::fs::write(&destination, "old content").unwrap();

        copy_file(&source, &destination, false).unwrap();

        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "new content"
        );
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".waysync-")));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn destination_expansion_supports_tilde_xdg_and_shell_fallbacks() {
        assert_eq!(
            destination_path("~/.config/waysync-test").unwrap(),
            dirs::home_dir().unwrap().join(".config/waysync-test")
        );
        assert_eq!(
            expand_env("${WAYSYNC_TEST_UNSET_99:-/tmp/default}"),
            "/tmp/default"
        );
        assert!(destination_path("$XDG_CONFIG_HOME/waysync-test")
            .unwrap()
            .is_absolute());
        for value in [
            "$HOME",
            "/etc/",
            "/",
            "/tmp",
            "relative/file",
            "/tmp/../etc/file",
            "$WAYSYNC_TEST_UNSET_99/file",
            "$HOME$WAYSYNC_TEST_UNSET_99/file",
            "${HOME/file",
        ] {
            assert!(destination_path(value).is_err(), "Should reject {value}");
        }
    }
}
