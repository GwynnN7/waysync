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
        .args(["clone", "--", url])
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

    pull_fast_forward(path)
}

pub fn ensure_repo_root(path: &Path) -> Result<()> {
    // Git otherwise walks upward and can accidentally pull an enclosing checkout.
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()?;
    if !output.status.success()
        || Path::new(String::from_utf8_lossy(&output.stdout).trim()).canonicalize()?
            != path.canonicalize()?
    {
        bail!(
            "Destination is not a Git repository root: {}",
            path.display()
        );
    }
    Ok(())
}

fn pull_fast_forward(path: &Path) -> Result<()> {
    ensure_repo_root(path)?;
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
