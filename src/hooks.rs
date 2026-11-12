use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::manifest::HookEntry;
use crate::profile::Profile;

pub fn run_hooks(
    hooks: &[HookEntry],
    label: &str,
    profile: Profile<'_>,
    repo_dir: &Path,
) -> Result<()> {
    let applicable: Vec<&HookEntry> = hooks.iter().filter(|h| h.applies_to(profile)).collect();

    for (i, hook) in applicable.iter().enumerate() {
        let cmd_str = hook.cmd();
        println!(
            "  Running {} hook [{}/{}]: {}",
            label,
            i + 1,
            applicable.len(),
            cmd_str
        );
        let status = Command::new("sh")
            .args(["-c", cmd_str])
            .env("WAYSYNC_REPO_DIR", repo_dir)
            .env("WAYSYNC_ENV", profile.environment.unwrap_or(""))
            .env(
                "WAYSYNC_DEVICE",
                profile
                    .device
                    .map(|device| device.to_string())
                    .unwrap_or_default(),
            )
            .current_dir(repo_dir)
            .status()
            .with_context(|| format!("Failed to run hook: {}", cmd_str))?;
        if !status.success() {
            bail!("{label} hook exited with {status}: {cmd_str}");
        }
    }
    Ok(())
}
