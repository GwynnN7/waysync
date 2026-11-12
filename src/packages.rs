use std::ffi::OsStr;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use console::style;
use dialoguer::Select;
use walkdir::WalkDir;

static LOCAL_BUILD_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

/// A throwaway copy of a local package recipe.
///
/// `makepkg` and `paru -Ui` write source checkouts, archives, `.SRCINFO`, and may
/// refresh the `pkgver` assignment in a PKGBUILD. Building from this copy keeps
/// the managed dotfiles repository clean and safe to pull.
struct LocalPackageBuildDir {
    path: PathBuf,
}

impl LocalPackageBuildDir {
    fn create(repo_path: &Path, name: &str) -> Result<Self> {
        let source = repo_path.join("packages").join(name);
        if !source.is_dir() {
            bail!("Local package directory not found: {}", source.display());
        }

        let path = create_temp_build_dir()?;
        if let Err(error) = copy_package_recipe(&source, &path) {
            let _ = fs::remove_dir_all(&path);
            return Err(error);
        }

        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LocalPackageBuildDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!(
                "Warning: could not remove temporary package build directory {}: {}",
                self.path.display(),
                error
            );
        }
    }
}

fn create_temp_build_dir() -> Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for _ in 0..32 {
        let sequence = LOCAL_BUILD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "waysync-package-build-{}-{}-{}",
            std::process::id(),
            timestamp,
            sequence
        ));

        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Could not create temporary package build directory {}",
                        path.display()
                    )
                });
            }
        }
    }

    bail!("Could not allocate a unique temporary package build directory")
}

fn copy_package_recipe(source: &Path, destination: &Path) -> Result<()> {
    for entry in WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let relative = entry
                .path()
                .strip_prefix(source)
                .expect("WalkDir entry should be inside its root");
            !(relative.components().count() == 1
                && entry.file_type().is_dir()
                && is_generated_build_entry(entry.path(), entry.file_type()))
        })
    {
        let entry = entry.with_context(|| {
            format!(
                "Could not read local package recipe at {}",
                source.display()
            )
        })?;
        let source_path = entry.path();
        let relative = source_path
            .strip_prefix(source)
            .expect("WalkDir entry should be inside its root");

        if relative.as_os_str().is_empty() {
            continue;
        }

        // An earlier Waysync version may have left build output behind. Do not
        // copy that output into this isolated build either.
        if relative.components().count() == 1
            && is_generated_build_entry(source_path, entry.file_type())
        {
            continue;
        }

        let destination_path = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination_path).with_context(|| {
                format!(
                    "Could not create package build directory {}",
                    destination_path.display()
                )
            })?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source_path, &destination_path).with_context(|| {
                format!(
                    "Could not copy package recipe file {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
            fs::set_permissions(&destination_path, fs::metadata(source_path)?.permissions())?;
        } else if entry.file_type().is_symlink() {
            copy_symlink(source_path, &destination_path)?;
        }
    }

    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    symlink(fs::read_link(source)?, destination).with_context(|| {
        format!(
            "Could not copy package recipe symlink {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(source: &Path, _destination: &Path) -> Result<()> {
    bail!(
        "Local package recipe contains unsupported symlink: {}",
        source.display()
    )
}

fn is_generated_build_entry(path: &Path, file_type: fs::FileType) -> bool {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };

    matches!(name, ".SRCINFO" | "src" | "pkg" | "pkgdir")
        || name.contains(".pkg.tar.")
        || name.contains(".src.tar.")
        // Source checkouts produced by VCS PKGBUILDs are usually named after
        // the package rather than `src`. Treat only untracked Git checkouts as
        // generated output, so normal recipe directories are never removed.
        || (file_type.is_dir() && path.join(".git").exists())
}

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
fn resolve_local_pkg_info_in_dir(package_dir: &Path, fallback_name: &str) -> LocalPkgInfo {
    if let Ok(output) = Command::new("makepkg")
        .arg("--printsrcinfo")
        .current_dir(package_dir)
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
        names: vec![fallback_name.to_string()],
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
fn local_package_needs_build(pkg_dir: &Path, name: &str) -> Result<bool> {
    if !pkg_dir.exists() {
        bail!("Local package directory not found: {}", pkg_dir.display());
    }

    let initial_info = resolve_local_pkg_info_in_dir(pkg_dir, name);

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
    if pkgbuild_has_pkgver_func(pkg_dir) {
        println!(
            "  Checking upstream sources for local package: {}...",
            style(name).cyan()
        );
        let _ = Command::new("makepkg")
            .args(["-od", "--noconfirm"])
            .current_dir(pkg_dir)
            .status();
    }

    let updated_info = resolve_local_pkg_info_in_dir(pkg_dir, name);
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
    if !std::io::stdin().is_terminal() {
        bail!("Package step failed for '{label}'; continuing with other components");
    }
    let choices = &[
        "Skip failed step and continue other components",
        "Retry interactive installation",
    ];
    let selection = Select::new()
        .with_prompt(format!(
            "Package step failed for '{}'. How to proceed?",
            label
        ))
        .items(choices)
        .default(0)
        .interact()?;

    match selection {
        1 => Ok(true), // Retry
        _ => bail!("Package step skipped after failure: {}", label),
    }
}

/// Unified runner for all package operations (Standard, AUR, and Local):
/// 1. Attempts automatic execution with `--noconfirm` (`auto_args`).
/// 2. If that fails (e.g. conflict or provider prompt), automatically drops into
///    interactive mode (`interactive_args`, without `--noconfirm`).
/// 3. If interactive mode also fails, default to skipping the failed step.
///
/// Errors are returned so the engine can continue without recording false success.
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

    if !std::io::stdin().is_terminal() {
        bail!("Package step failed for '{label}' (no interactive terminal)");
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
    let temporary = tempfile::tempdir()?;
    let build_path = temporary.path().join("paru-bin");
    let tmp = build_path.as_path();
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
        } else {
            base_args.push("--rebuild".to_string());
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
        } else {
            base_args.push("--rebuild".to_string());
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
        let build_dir = LocalPackageBuildDir::create(repo_path, &name)?;

        if needed && !local_package_needs_build(build_dir.path(), &name)? {
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
            Some(build_dir.path()),
        )?;
    }

    Ok(())
}

/// Resolve manifest package specs to the concrete package names managed by pacman.
///
/// This makes `foo`, `aur:foo`, and a local PKGBUILD that produces `foo` compare as
/// the same package when deciding whether it is still needed by another component.
pub fn package_names_for_specs(packages: &[String], repo_path: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for spec in packages {
        names.extend(names_for_package_audit(spec, repo_path)?);
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// Let libalpm plan the whole removal transaction, including versioned and virtual
/// dependencies. Keep targets required by packages outside this transaction.
fn removable_packages(names: &[String]) -> Result<Vec<String>> {
    let output = Command::new("pacman")
        .args(["-R", "--unneeded", "--print", "--print-format", "%n", "--"])
        .args(names)
        .env("LC_ALL", "C")
        .output()
        .context("Failed to plan dependency-safe package removal")?;
    if !output.status.success() {
        bail!(
            "Could not plan package removal: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut removable = Vec::new();
    for line in String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "there is nothing to do")
    {
        if !names.iter().any(|name| name == line) {
            bail!("Unexpected package in removal plan: {line}");
        }
        removable.push(line.to_string());
    }
    removable.sort();
    removable.dedup();
    Ok(removable)
}

/// Remove only candidates that no installed package still needs. Never cascade,
/// disable dependency checks, or recursively remove dependencies/orphans.
pub fn uninstall_package_names(package_names: &[String]) -> Result<()> {
    if package_names.is_empty() {
        return Ok(());
    }
    let installed = installed_package_names()?;
    let mut names: Vec<_> = package_names
        .iter()
        .filter(|name| installed.contains(*name))
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return Ok(());
    }
    let removable = removable_packages(&names)?;
    for name in names.iter().filter(|name| !removable.contains(name)) {
        println!("  Keeping {name}: required by another installed package.");
    }
    if removable.is_empty() {
        return Ok(());
    }
    println!("  Uninstalling unneeded packages: {}", removable.join(", "));
    let mut automatic = vec![
        "pacman".to_string(),
        "-R".to_string(),
        "--unneeded".to_string(),
        "--noconfirm".to_string(),
        "--".to_string(),
    ];
    automatic.extend(removable.clone());
    let mut interactive = vec![
        "pacman".to_string(),
        "-R".to_string(),
        "--unneeded".to_string(),
        "--".to_string(),
    ];
    interactive.extend(removable);
    run_with_interactive_fallback(
        "dependency-safe package removal",
        "sudo",
        &automatic,
        &interactive,
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::package_names_for_specs;

    fn temporary_test_root(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "waysync-{}-{}-{}",
            label,
            std::process::id(),
            unique
        ))
    }

    #[test]
    fn local_specs_resolve_to_the_package_names_they_produce() {
        let root = temporary_test_root("package-spec-test");
        let package_dir = root.join("packages/theme");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::write(
            package_dir.join("PKGBUILD"),
            "pkgname=('shared-theme' 'theme-extra')\npkgver=1\npkgrel=1\narch=('any')\npackage_shared-theme() { :; }\npackage_theme-extra() { :; }\n",
        )
        .unwrap();

        let names =
            package_names_for_specs(&["aur:shared-theme".into(), "local:theme".into()], &root)
                .unwrap();

        assert_eq!(names, vec!["shared-theme", "theme-extra"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod recipe_input_tests {
    use super::*;

    #[test]
    fn isolated_builds_preserve_bundled_source_archives() {
        let (_root, config) = crate::test_support::fixture();
        let recipe = config.repo_dir.join("packages/example");
        crate::test_support::write(recipe.join("PKGBUILD"), "pkgname=example\n");
        crate::test_support::write(recipe.join("bundled-source.zip"), "recipe input");
        crate::test_support::write(recipe.join("src/generated"), "build output");
        let build = LocalPackageBuildDir::create(&config.repo_dir, "example").unwrap();
        assert_eq!(
            fs::read_to_string(build.path().join("bundled-source.zip")).unwrap(),
            "recipe input"
        );
        assert!(!build.path().join("src").exists());
        assert!(recipe.join("src/generated").exists());
    }
}

/// Query the actual package database once per reconciliation, without escalating.
/// Errors are not treated as missing packages: a broken query must not trigger installs.
pub fn installed_package_names() -> Result<std::collections::HashSet<String>> {
    let output = Command::new("pacman")
        .arg("-Qq")
        .output()
        .context("Failed to query installed packages with pacman -Qq")?;
    if !output.status.success() {
        bail!(
            "Could not query installed packages: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("Invalid package database output")?
        .lines()
        .map(str::to_string)
        .collect())
}

/// Resolve split local packages from isolated metadata; never assume the recipe
/// directory is the installed package name when deciding whether a repair is needed.
pub fn names_for_package_audit(spec: &str, repo_path: &Path) -> Result<Vec<String>> {
    match parse_package_spec(spec) {
        PackageSpec::Standard(name) => {
            Ok(vec![name.rsplit('/').next().unwrap_or(&name).to_string()])
        }
        PackageSpec::Aur(name) => Ok(vec![name]),
        PackageSpec::Local(name) => {
            let build = LocalPackageBuildDir::create(repo_path, &name)?;
            let output = Command::new("makepkg")
                .arg("--printsrcinfo")
                .current_dir(build.path())
                .output()
                .with_context(|| format!("Failed to read package names for local:{name}"))?;
            if !output.status.success() {
                bail!(
                    "Could not read package names for local:{name}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let names: Vec<_> = String::from_utf8(output.stdout)?
                .lines()
                .filter_map(|line| line.trim().strip_prefix("pkgname = ").map(str::to_string))
                .filter(|name| {
                    !name.is_empty()
                        && !name.starts_with('-')
                        && !name.contains(char::is_whitespace)
                })
                .collect();
            if names.is_empty() {
                bail!("No package names in metadata for local:{name}");
            }
            Ok(names)
        }
    }
}

/// Accept installed stable/Git counterparts as requested by this personal setup,
/// then let libalpm handle declared Provides and versioned requirements.
pub fn requirement_satisfied(
    name: &str,
    installed: &std::collections::HashSet<String>,
) -> Result<bool> {
    if installed.contains(name) {
        return Ok(true);
    }
    let versioned = name.contains(['<', '>', '=']);
    if !versioned {
        let alternate = name
            .strip_suffix("-git")
            .map(str::to_string)
            .unwrap_or_else(|| format!("{name}-git"));
        if installed.contains(&alternate) {
            return Ok(true);
        }
    }
    let output = Command::new("pacman")
        .args(["-T", name])
        .output()
        .context("Failed to check installed package providers")?;
    match output.status.code() {
        Some(0) if output.stdout.is_empty() => Ok(true),
        Some(127) => Ok(false),
        _ => bail!(
            "Could not check providers for {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

pub fn missing_specs(specs: &[String], repo: &Path) -> Result<Vec<String>> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let installed = installed_package_names()?;
    let mut missing = Vec::new();
    for spec in specs {
        let mut satisfied = true;
        for name in names_for_package_audit(spec, repo)? {
            if !requirement_satisfied(&name, &installed)? {
                satisfied = false;
            }
        }
        if !satisfied {
            missing.push(spec.clone());
        }
    }
    Ok(missing)
}
