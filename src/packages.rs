use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use console::style;
use dialoguer::Select;

/// Parsed package specification.
pub enum PackageSpec {
    /// Standard package (native or AUR), installed via `paru -S`.
    Standard(String),
    /// Explicit AUR package (`aur:<name>`), installed via `paru -S --aur`.
    Aur(String),
    /// Local PKGBUILD package (`local:<name>`), located in `packages/{name}/`.
    Local(String),
}

/// Metadata extracted from a local package's `.SRCINFO` / `PKGBUILD`.
struct LocalPkgInfo {
    names: Vec<String>,
    version: Option<String>,
}

/// Parse a package spec string.
pub fn parse_package_spec(spec: &str) -> PackageSpec {
    if let Some(name) = spec.strip_prefix("local:") {
        PackageSpec::Local(name.to_string())
    } else if let Some(name) = spec.strip_prefix("aur:") {
        PackageSpec::Aur(name.to_string())
    } else {
        PackageSpec::Standard(spec.to_string())
    }
}

/// Check if a PKGBUILD defines a dynamic `pkgver()` function.
fn pkgbuild_has_pkgver_func(package_dir: &Path) -> bool {
    let pkgbuild_path = package_dir.join("PKGBUILD");
    if let Ok(content) = std::fs::read_to_string(pkgbuild_path) {
        content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("pkgver()") || trimmed.starts_with("function pkgver")
        })
    } else {
        false
    }
}

/// Extract package names and full target version (`[epoch:]pkgver-pkgrel`) from `packages/{dir_name}`.
fn resolve_local_pkg_info(repo_path: &Path, dir_name: &str) -> LocalPkgInfo {
    let package_dir = repo_path.join("packages").join(dir_name);
    if let Ok(output) = Command::new("makepkg")
        .arg("--printsrcinfo")
        .current_dir(&package_dir)
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut names = Vec::new();
            let mut pkgver = None;
            let mut pkgrel = None;
            let mut epoch = None;

            for line in stdout.lines() {
                let trimmed = line.trim();
                if let Some(name) = trimmed.strip_prefix("pkgname = ") {
                    names.push(name.to_owned());
                } else if let Some(ver) = trimmed.strip_prefix("pkgver = ") {
                    pkgver = Some(ver.to_owned());
                } else if let Some(rel) = trimmed.strip_prefix("pkgrel = ") {
                    pkgrel = Some(rel.to_owned());
                } else if let Some(ep) = trimmed.strip_prefix("epoch = ") {
                    if !ep.is_empty() && ep != "0" {
                        epoch = Some(ep.to_owned());
                    }
                }
            }

            let version = match (epoch, pkgver, pkgrel) {
                (Some(ep), Some(ver), Some(rel)) => Some(format!("{}:{}-{}", ep, ver, rel)),
                (None, Some(ver), Some(rel)) => Some(format!("{}-{}", ver, rel)),
                _ => None,
            };

            if !names.is_empty() {
                return LocalPkgInfo { names, version };
            }
        }
    }

    let pkgbuild_path = package_dir.join("PKGBUILD");
    if let Ok(content) = std::fs::read_to_string(&pkgbuild_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("pkgname=") {
                let cleaned = rest.trim_matches(|c| c == '(' || c == ')' || c == '"' || c == '\'');
                let names: Vec<String> = cleaned
                    .split_whitespace()
                    .map(|s| s.trim_matches(|c| c == '"' || c == '\'').to_string())
                    .filter(|s| !s.is_empty() && !s.contains('\x24'))
                    .collect();
                if !names.is_empty() {
                    return LocalPkgInfo {
                        names,
                        version: None,
                    };
                }
            }
        }
    }

    LocalPkgInfo {
        names: vec![dir_name.to_string()],
        version: None,
    }
}

/// Get the currently installed version of a package via `pacman -Q <name>`.
fn get_installed_version(name: &str) -> Option<String> {
    let output = Command::new("pacman")
        .args(["-Q", name])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.split_whitespace().nth(1).map(str::to_owned)
}

/// Compare two Arch package versions using `vercmp`.
/// Returns `true` if `installed` is older than `target` (`vercmp` < 0).
fn is_version_older(installed: &str, target: &str) -> bool {
    if let Ok(output) = Command::new("vercmp").args([installed, target]).output() {
        if output.status.success() {
            let cmp = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<i32>()
                .unwrap_or(0);
            return cmp < 0;
        }
    }
    installed != target
}

/// Check whether a local package in `packages/{name}` needs to be built/updated.
fn local_package_needs_build(repo_path: &Path, name: &str) -> Result<bool> {
    let pkg_dir = repo_path.join("packages").join(name);
    if !pkg_dir.exists() {
        bail!("Local package directory not found: {}", pkg_dir.display());
    }

    let initial_info = resolve_local_pkg_info(repo_path, name);

    // If any split/target package is not installed yet, we must build it.
    if initial_info
        .names
        .iter()
        .any(|p| !is_exact_package_installed(p))
    {
        return Ok(true);
    }

    // If the PKGBUILD has a dynamic `pkgver()` function (e.g. git package),
    // run `makepkg -od --noconfirm` to fetch latest sources and refresh `pkgver`
    // without compiling.
    if pkgbuild_has_pkgver_func(&pkg_dir) {
        println!(
            "  Checking upstream sources for local package: {}...",
            style(name).cyan()
        );
        let _ = Command::new("makepkg")
            .args(["-od", "--noconfirm"])
            .current_dir(&pkg_dir)
            .status();
    }

    let updated_info = resolve_local_pkg_info(repo_path, name);
    let Some(target_ver) = updated_info.version else {
        return Ok(true);
    };

    for pkg in &updated_info.names {
        match get_installed_version(pkg) {
            Some(installed_ver) => {
                if is_version_older(&installed_ver, &target_ver) {
                    println!(
                        "  Update available for {} ({} → {})",
                        style(pkg).cyan(),
                        style(installed_ver).dim(),
                        style(&target_ver).green()
                    );
                    return Ok(true);
                }
            }
            None => return Ok(true),
        }
    }

    println!(
        "  {} is already up to date ({}), skipping",
        name,
        style(&target_ver).dim()
    );
    Ok(false)
}

/// Ask user how to proceed if an interactive package installation still fails.
fn prompt_package_failure(label: &str) -> Result<bool> {
    let choices = &[
        "Skip this package step and continue deployment",
        "Retry interactive installation",
        "Abort deployment",
    ];
    let selection = Select::new()
        .with_prompt(format!("Package step failed for '{}'. How to proceed?", label))
        .items(choices)
        .default(0)
        .interact()?;

    match selection {
        0 => Ok(false), // Skip and continue
        1 => Ok(true),  // Retry
        _ => bail!("Deployment aborted by user after package failure: {}", label),
    }
}

/// Unified runner for all package operations (Standard, AUR, and Local):
/// 1. Attempts automatic execution with `--noconfirm` (`auto_args`).
/// 2. If that fails (e.g. conflict or provider prompt), automatically drops into
///    interactive mode (`interactive_args`, without `--noconfirm`).
/// 3. If interactive mode also fails, prompts the user to Skip, Retry, or Abort.
fn run_with_interactive_fallback(
    label: &str,
    program: &str,
    auto_args: &[String],
    interactive_args: &[String],
    current_dir: Option<&Path>,
) -> Result<()> {
    let mut command = Command::new(program);
    command.args(auto_args);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }
    let status = command
        .status()
        .with_context(|| format!("Failed to run {} for {}", program, label))?;

    if status.success() {
        return Ok(());
    }

    println!(
        "  {} Automatic install failed for '{}'. Retrying interactively...",
        style("!").yellow().bold(),
        style(label).yellow()
    );

    loop {
        let mut retry_command = Command::new(program);
        retry_command.args(interactive_args);
        if let Some(dir) = current_dir {
            retry_command.current_dir(dir);
        }
        let retry = retry_command
            .status()
            .with_context(|| format!("Failed to run interactive {} for {}", program, label))?;

        if retry.success() {
            return Ok(());
        }

        if !prompt_package_failure(label)? {
            println!("  {} Skipping {}...", style("→").dim(), label);
            return Ok(());
        }
    }
}

/// Ensure paru (AUR helper) is installed.
pub fn ensure_paru() -> Result<()> {
    if Command::new("paru")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Ok(());
    }

    println!("paru not found, installing paru-bin...");
    let tmp = Path::new("/tmp/waysync-paru-build");
    let _ = std::fs::remove_dir_all(tmp);
    let status = Command::new("git")
        .args(["clone", "https://aur.archlinux.org/paru-bin.git"])
        .arg(tmp)
        .status()
        .context("Failed to clone paru-bin")?;
    if !status.success() {
        bail!("Failed to clone paru-bin from AUR");
    }

    let auto_args = vec!["-si".to_string(), "--noconfirm".to_string()];
    let interactive_args = vec!["-si".to_string()];
    let res = run_with_interactive_fallback(
        "paru-bin",
        "makepkg",
        &auto_args,
        &interactive_args,
        Some(tmp),
    );
    let _ = std::fs::remove_dir_all(tmp);
    res?;

    println!("paru installed successfully.");
    Ok(())
}

/// Check if an exact package name is installed via pacman (`pacman -Qq`).
fn is_exact_package_installed(name: &str) -> bool {
    Command::new("pacman")
        .args(["-Qq", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Install packages. `repo_path` is the dotfiles repo root (for local PKGBUILDs).
pub fn install_packages(packages: &[String], repo_path: &Path, needed: bool) -> Result<()> {
    let mut standard = Vec::new();
    let mut aur = Vec::new();
    let mut local = Vec::new();

    for spec in packages {
        match parse_package_spec(spec) {
            PackageSpec::Standard(name) => standard.push(name),
            PackageSpec::Aur(name) => aur.push(name),
            PackageSpec::Local(name) => local.push(name),
        }
    }

    // 1. Explicit AUR packages (`aur:<name>`)
    for pkg in aur {
        let label = format!("aur:{}", pkg);
        println!("  Installing/checking AUR package: {}", pkg);

        let mut base_args = vec!["-S".to_string(), "--aur".to_string()];
        if needed {
            base_args.push("--needed".to_string());
        }

        let mut auto_args = base_args.clone();
        auto_args.push("--noconfirm".to_string());
        auto_args.push(pkg.clone());

        let mut interactive_args = base_args;
        interactive_args.push(pkg);

        run_with_interactive_fallback(&label, "paru", &auto_args, &interactive_args, None)?;
    }

    // 2. Standard packages (native or AUR via `paru -S`)
    if !standard.is_empty() {
        let label = standard.join(", ");
        println!("  Installing/checking packages: {}", label);

        let mut base_args = vec!["-S".to_string()];
        if needed {
            base_args.push("--needed".to_string());
        }

        let mut auto_args = base_args.clone();
        auto_args.push("--noconfirm".to_string());
        auto_args.extend(standard.clone());

        let mut interactive_args = base_args;
        interactive_args.extend(standard);

        run_with_interactive_fallback(&label, "paru", &auto_args, &interactive_args, None)?;
    }

    // 3. Local PKGBUILD packages (`local:<name>`) built via `paru -Ui`
    for name in local {
        let pkg_dir = repo_path.join("packages").join(&name);
        if !pkg_dir.exists() {
            bail!("Local package directory not found: {}", pkg_dir.display());
        }

        if needed && !local_package_needs_build(repo_path, &name)? {
            continue;
        }

        let label = format!("local:{}", name);
        println!("  Building local package: {}", name);

        let mut base_args = vec!["-Ui".to_string()];
        if !needed {
            base_args.push("--rebuild".to_string());
        }

        let mut auto_args = base_args.clone();
        auto_args.push("--noconfirm".to_string());

        let interactive_args = base_args;

        run_with_interactive_fallback(
            &label,
            "paru",
            &auto_args,
            &interactive_args,
            Some(&pkg_dir),
        )?;
    }

    Ok(())
}

/// Uninstall packages. Silently skips packages that aren't installed.
pub fn uninstall_packages(packages: &[String], repo_path: &Path) -> Result<()> {
    let mut names = Vec::new();
    for spec in packages {
        match parse_package_spec(spec) {
            PackageSpec::Standard(name) | PackageSpec::Aur(name) => {
                if is_exact_package_installed(&name) {
                    names.push(name);
                }
            }
            PackageSpec::Local(dir_name) => {
                for pkg in resolve_local_pkg_info(repo_path, &dir_name).names {
                    if is_exact_package_installed(&pkg) {
                        names.push(pkg);
                    }
                }
            }
        }
    }

    if names.is_empty() {
        return Ok(());
    }

    let label = format!("uninstall ({})", names.join(", "));
    println!("  Uninstalling packages: {}", names.join(", "));

    let mut auto_args = vec![
        "pacman".to_string(),
        "-Rns".to_string(),
        "--noconfirm".to_string(),
    ];
    auto_args.extend(names.clone());

    let mut interactive_args = vec!["pacman".to_string(), "-Rns".to_string()];
    interactive_args.extend(names);

    run_with_interactive_fallback(&label, "sudo", &auto_args, &interactive_args, None)
}