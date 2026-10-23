use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
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

/// Delete untracked files and directories that `makepkg`/`paru -Ui` generate in
/// `packages/*`. Tracked recipe files and any unrecognised local files are left
/// untouched.
pub fn clean_local_package_build_artifacts(repo_path: &Path) -> Result<usize> {
    let packages_dir = repo_path.join("packages");
    if !packages_dir.is_dir() {
        return Ok(0);
    }

    let tracked_paths = tracked_package_paths(repo_path)?;
    let mut removed = 0;

    for entry in fs::read_dir(&packages_dir).with_context(|| {
        format!(
            "Could not read package directory {}",
            packages_dir.display()
        )
    })? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let package_name = entry.file_name();
        let package_dir = entry.path();
        let package_relative = Path::new("packages").join(&package_name);
        removed +=
            clean_package_build_artifacts_in_dir(&package_dir, &package_relative, &tracked_paths)?;
    }

    Ok(removed)
}

/// Restore only PKGBUILDs whose sole local change is an auto-generated `pkgver`
/// update. This lets a repository refresh continue while preserving all other
/// local recipe edits.
pub fn restore_generated_pkgver_updates(repo_path: &Path) -> Result<usize> {
    let changed = Command::new("git")
        .args(["diff", "--name-only", "--", "packages"])
        .current_dir(repo_path)
        .output()
        .context("Failed to inspect local PKGBUILD changes")?;
    if !changed.status.success() {
        bail!(
            "Could not inspect local PKGBUILD changes at {}",
            repo_path.display()
        );
    }

    let mut restored = 0;
    for path in String::from_utf8_lossy(&changed.stdout).lines() {
        if !path.starts_with("packages/") || !path.ends_with("/PKGBUILD") {
            continue;
        }

        let diff = Command::new("git")
            .args(["diff", "--no-ext-diff", "--unified=0", "--", path])
            .current_dir(repo_path)
            .output()
            .with_context(|| format!("Failed to inspect local change to {}", path))?;
        if !diff.status.success() {
            bail!("Could not inspect local change to {}", path);
        }

        if !is_generated_pkgver_only_diff(&String::from_utf8_lossy(&diff.stdout)) {
            continue;
        }

        let status = Command::new("git")
            .args(["restore", "--source=HEAD", "--worktree", "--", path])
            .current_dir(repo_path)
            .status()
            .with_context(|| format!("Failed to restore generated PKGBUILD version in {}", path))?;
        if !status.success() {
            bail!("Could not restore generated PKGBUILD version in {}", path);
        }
        restored += 1;
    }

    Ok(restored)
}

fn tracked_package_paths(repo_path: &Path) -> Result<HashSet<String>> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "packages"])
        .current_dir(repo_path)
        .output()
        .context("Failed to inspect tracked package recipe files")?;
    if !output.status.success() {
        bail!(
            "Could not inspect tracked package recipe files at {}",
            repo_path.display()
        );
    }

    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

fn clean_package_build_artifacts_in_dir(
    package_dir: &Path,
    package_relative: &Path,
    tracked_paths: &HashSet<String>,
) -> Result<usize> {
    let mut removed = 0;

    for entry in fs::read_dir(package_dir)
        .with_context(|| format!("Could not read package directory {}", package_dir.display()))?
    {
        let entry = entry?;
        let artifact_path = entry.path();
        let file_type = entry.file_type()?;
        if !is_generated_build_entry(&artifact_path, file_type) {
            continue;
        }

        let relative = package_relative.join(entry.file_name());
        if path_is_tracked_or_contains_tracked_files(&relative, tracked_paths) {
            continue;
        }

        remove_build_artifact(&artifact_path, file_type)?;
        removed += 1;
    }

    Ok(removed)
}

fn is_generated_build_entry(path: &Path, file_type: fs::FileType) -> bool {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };

    matches!(name, ".SRCINFO" | "src" | "pkg" | "pkgdir")
        || name.contains(".pkg.tar.")
        || name.contains(".src.tar.")
        || is_downloaded_source_archive(name)
        // Source checkouts produced by VCS PKGBUILDs are usually named after
        // the package rather than `src`. Treat only untracked Git checkouts as
        // generated output, so normal recipe directories are never removed.
        || (file_type.is_dir() && path.join(".git").exists())
}

fn is_downloaded_source_archive(name: &str) -> bool {
    name.ends_with(".zip")
        || name.ends_with(".tar")
        || name.contains(".tar.gz")
        || name.contains(".tar.bz2")
        || name.contains(".tar.xz")
        || name.contains(".tar.zst")
}

fn path_is_tracked_or_contains_tracked_files(path: &Path, tracked_paths: &HashSet<String>) -> bool {
    let path = path.to_string_lossy();
    let prefix = format!("{}/", path);
    tracked_paths
        .iter()
        .any(|tracked| tracked == path.as_ref() || tracked.starts_with(&prefix))
}

fn remove_build_artifact(path: &Path, file_type: fs::FileType) -> Result<()> {
    if file_type.is_dir() {
        fs::remove_dir_all(path).with_context(|| {
            format!(
                "Could not remove package build directory {}",
                path.display()
            )
        })?;
    } else {
        fs::remove_file(path).with_context(|| {
            format!("Could not remove package build artifact {}", path.display())
        })?;
    }
    Ok(())
}

fn is_generated_pkgver_only_diff(diff: &str) -> bool {
    let mut has_change = false;

    for line in diff.lines() {
        if line.starts_with("diff --git ")
            || line.starts_with("index ")
            || line.starts_with("@@ ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            continue;
        }

        if let Some(change) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) {
            has_change = true;
            if !change.trim_start().starts_with("pkgver=") {
                return false;
            }
        }
    }

    has_change
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
fn resolve_local_pkg_info_in_dir(package_dir: &Path, fallback_name: &str) -> LocalPkgInfo {
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
        names: vec![fallback_name.to_string()],
        version: None,
    }
}

fn resolve_local_pkg_info(repo_path: &Path, dir_name: &str) -> LocalPkgInfo {
    let package_dir = repo_path.join("packages").join(dir_name);
    resolve_local_pkg_info_in_dir(&package_dir, dir_name)
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
    let choices = &[
        "Skip this package step and continue deployment",
        "Retry interactive installation",
        "Abort deployment",
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
        0 => Ok(false), // Skip and continue
        1 => Ok(true),  // Retry
        _ => bail!(
            "Deployment aborted by user after package failure: {}",
            label
        ),
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
pub fn package_names_for_specs(packages: &[String], repo_path: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for spec in packages {
        match parse_package_spec(spec) {
            PackageSpec::Standard(name) | PackageSpec::Aur(name) => {
                names.push(name);
            }
            PackageSpec::Local(dir_name) => {
                names.extend(resolve_local_pkg_info(repo_path, &dir_name).names);
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Uninstall concrete package names. Silently skips packages that aren't installed.
///
/// With `remove_orphaned_dependencies` disabled, pacman removes only the named
/// packages. This is used during an environment change so one component's teardown
/// cannot sweep dependencies that another enabled component still relies on.
pub fn uninstall_package_names(
    package_names: &[String],
    remove_orphaned_dependencies: bool,
) -> Result<()> {
    let mut names: Vec<_> = package_names
        .iter()
        .filter(|name| is_exact_package_installed(name))
        .cloned()
        .collect();
    names.sort();
    names.dedup();

    if names.is_empty() {
        return Ok(());
    }

    let label = format!("uninstall ({})", names.join(", "));
    println!("  Uninstalling packages: {}", names.join(", "));

    let removal_mode = removal_mode(remove_orphaned_dependencies);
    let mut auto_args = vec![
        "pacman".to_string(),
        removal_mode.to_string(),
        "--noconfirm".to_string(),
    ];
    auto_args.extend(names.clone());

    let mut interactive_args = vec!["pacman".to_string(), removal_mode.to_string()];
    interactive_args.extend(names);

    run_with_interactive_fallback(&label, "sudo", &auto_args, &interactive_args, None)
}

fn removal_mode(remove_orphaned_dependencies: bool) -> &'static str {
    if remove_orphaned_dependencies {
        "-Rns"
    } else {
        "-R"
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        clean_package_build_artifacts_in_dir, is_generated_pkgver_only_diff,
        package_names_for_specs, removal_mode,
    };

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
            "pkgname=('shared-theme' 'theme-extra')\npkgver=1\npkgrel=1\narch=('any')\n",
        )
        .unwrap();

        let names =
            package_names_for_specs(&["aur:shared-theme".into(), "local:theme".into()], &root);

        assert_eq!(names, vec!["shared-theme", "theme-extra"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn environment_changes_do_not_request_recursive_dependency_removal() {
        assert_eq!(removal_mode(false), "-R");
        assert_eq!(removal_mode(true), "-Rns");
    }

    #[test]
    fn cleanup_removes_only_untracked_makepkg_artifacts() {
        let root = temporary_test_root("package-cleanup-test");
        let package_dir = root.join("packages/theme");
        std::fs::create_dir_all(package_dir.join("src")).unwrap();
        std::fs::create_dir_all(package_dir.join("theme-git/.git")).unwrap();
        std::fs::write(package_dir.join("PKGBUILD"), "pkgname=theme\n").unwrap();
        std::fs::write(package_dir.join(".SRCINFO"), "generated\n").unwrap();
        std::fs::write(package_dir.join("theme-1-1-any.pkg.tar.zst"), "archive\n").unwrap();
        std::fs::write(package_dir.join("upstream.zip"), "source archive\n").unwrap();
        std::fs::write(package_dir.join("patch.diff"), "manual input\n").unwrap();

        let tracked = HashSet::from(["packages/theme/PKGBUILD".to_string()]);
        let removed = clean_package_build_artifacts_in_dir(
            &package_dir,
            std::path::Path::new("packages/theme"),
            &tracked,
        )
        .unwrap();

        assert_eq!(removed, 5);
        assert!(package_dir.join("PKGBUILD").exists());
        assert!(package_dir.join("patch.diff").exists());
        assert!(!package_dir.join(".SRCINFO").exists());
        assert!(!package_dir.join("src").exists());
        assert!(!package_dir.join("theme-git").exists());
        assert!(!package_dir.join("theme-1-1-any.pkg.tar.zst").exists());
        assert!(!package_dir.join("upstream.zip").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_a_pkgver_change_is_safe_to_restore_before_pulling() {
        assert!(is_generated_pkgver_only_diff(
            "diff --git a/packages/theme/PKGBUILD b/packages/theme/PKGBUILD\n\
             index 123..456 100644\n\
             --- a/packages/theme/PKGBUILD\n\
             +++ b/packages/theme/PKGBUILD\n\
             @@ -1 +1 @@\n\
             -pkgver=1\n\
             +pkgver=2\n"
        ));
        assert!(!is_generated_pkgver_only_diff(
            "@@ -1,2 +1,2 @@\n\
             -pkgver=1\n\
             +pkgver=2\n\
             -depends=('old')\n\
             +depends=('new')\n"
        ));
    }
}
