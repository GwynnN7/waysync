use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Clone a git repository to the given path.
pub fn clone_repo(url: &str, path: &Path) -> Result<()> {
    println!("Cloning {} ...", url);
    if path.exists() {
        bail!("Repository already exists at {}", path.display());
    }
    let status = Command::new("git")
        .args(["clone", url])
        .arg(path)
        .status()
        .context("Failed to run git clone")?;
    if !status.success() {
        bail!("git clone failed for {}", url);
    }
    Ok(())
}

/// Pull the latest changes in a generic git repository.
pub fn pull_repo(path: &Path) -> Result<()> {
    println!("Updating repository at {} ...", path.display());

    pull_repo_after_preparation(path)
}

/// Pull the managed Waysync repository after removing generated local package
/// build output. This intentionally does not affect repositories deployed as
/// ordinary file sources.
pub fn pull_managed_repo(path: &Path) -> Result<()> {
    println!("Updating repository at {} ...", path.display());

    let removed_artifacts = crate::packages::clean_local_package_build_artifacts(path)?;
    if removed_artifacts > 0 {
        println!(
            "Removed {} generated local package build artifact{}.",
            removed_artifacts,
            if removed_artifacts == 1 { "" } else { "s" }
        );
    }

    let restored_pkgvers = crate::packages::restore_generated_pkgver_updates(path)?;
    if restored_pkgvers > 0 {
        println!(
            "Restored {} generated PKGBUILD version update{}.",
            restored_pkgvers,
            if restored_pkgvers == 1 { "" } else { "s" }
        );
    }

    pull_repo_after_preparation(path)
}

fn pull_repo_after_preparation(path: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(path)
        .status()
        .context("Failed to run git pull")?;
    if !status.success() {
        bail!("git pull failed at {}", path.display());
    }
    Ok(())
}
