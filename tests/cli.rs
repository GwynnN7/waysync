use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_waysync"))
        .args(arguments)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .output()
        .unwrap()
}

#[test]
fn check_example_is_read_only_and_all_combinations_resolve() {
    let root = tempfile::tempdir().unwrap();
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/dotfiles");
    let output = run(
        root.path(),
        &["check", "--repo", example.to_str().unwrap(), "--all"],
    );
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.path().join("state").exists());
    assert!(!root.path().join("data").exists());
    assert!(!root.path().join("config").exists());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("Checking environment=")
            .count(),
        9
    );
}

#[test]
fn sync_and_direct_device_switch_apply_layered_files_without_touching_real_config() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("data/waysync/repo");
    fs::create_dir_all(repo.join("dotfiles")).unwrap();
    fs::create_dir_all(repo.join("devices/laptop/dotfiles")).unwrap();
    fs::create_dir_all(root.path().join("state/waysync")).unwrap();
    fs::write(repo.join("manifest.toml"), "[[components]]\nname='config'\n[[components.files]]\nsrc='settings'\ndest='$XDG_CONFIG_HOME/test/settings'\n").unwrap();
    fs::write(repo.join("dotfiles/settings"), "base").unwrap();
    fs::write(repo.join("devices/laptop/dotfiles/settings"), "laptop").unwrap();
    fs::write(
        root.path().join("state/waysync/state.toml"),
        "[components]\nconfig=true\n",
    )
    .unwrap();
    for (arguments, expected) in [
        (vec!["sync"], "base"),
        (vec!["device", "laptop"], "laptop"),
        (vec!["device", "desktop"], "base"),
        (vec!["device", "none"], "base"),
    ] {
        let output = run(root.path(), &arguments);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(root.path().join("config/test/settings")).unwrap(),
            expected
        );
    }
    let status = run(root.path(), &["status"]);
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("Device: none"));
}

#[test]
fn read_only_commands_do_not_create_directories_when_uninitialized() {
    let root = tempfile::tempdir().unwrap();
    for command in ["status", "check"] {
        assert!(!run(root.path(), &[command]).status.success());
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Waysync Test",
            "-c",
            "user.email=waysync@example.invalid",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
}

#[test]
fn update_only_refreshes_git_until_deploy_is_explicitly_requested() {
    let root = tempfile::tempdir().unwrap();
    let origin = root.path().join("origin");
    fs::create_dir_all(origin.join("dotfiles")).unwrap();
    fs::write(origin.join("manifest.toml"), "[[components]]\nname='config'\n[[components.files]]\nsrc='settings'\ndest='$XDG_CONFIG_HOME/test/settings'\n").unwrap();
    fs::write(origin.join("dotfiles/settings"), "original").unwrap();
    git(&origin, &["init", "-b", "main"]);
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-m", "initial"]);
    assert!(run(root.path(), &["init", origin.to_str().unwrap()])
        .status
        .success());
    assert!(run(root.path(), &["install", "config"]).status.success());
    fs::write(origin.join("dotfiles/settings"), "updated").unwrap();
    git(&origin, &["commit", "-am", "change"]);
    assert!(run(root.path(), &["update"]).status.success());
    assert_eq!(
        fs::read_to_string(root.path().join("config/test/settings")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("data/waysync/repo/dotfiles/settings")).unwrap(),
        "updated"
    );
    assert!(run(root.path(), &["deploy", "--no-hooks"]).status.success());
    assert_eq!(
        fs::read_to_string(root.path().join("config/test/settings")).unwrap(),
        "updated"
    );
}

#[test]
fn package_deployment_and_hook_scopes_are_independent() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("data/waysync/repo");
    let tools = root.path().join("tools");
    let calls = root.path().join("calls");
    let hooks = root.path().join("hooks");
    fs::create_dir_all(repo.join("dotfiles")).unwrap();
    fs::create_dir_all(repo.join("packages/recipe")).unwrap();
    fs::create_dir_all(&tools).unwrap();
    std::os::unix::fs::symlink("/bin/sh", tools.join("sh")).unwrap();
    for name in ["paru", "sudo", "makepkg", "pacman"] {
        let path = tools.join(name);
        fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{name} ' \"$*\" >> '{}'\nif [ '{name}' = makepkg ]; then printf 'pkgname = local-example\\npkgver = 1\\npkgrel = 1\\n'; fi\nif [ '{name}' = pacman ]; then printf 'native-example\\naur-example\\nlocal-example\\n'; fi\nexit 0\n", calls.display())).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        repo.join("packages/recipe/PKGBUILD"),
        "pkgname=local-example\npkgver=1\npkgrel=1\n",
    )
    .unwrap();
    fs::write(repo.join("dotfiles/settings"), "original").unwrap();
    fs::write(
        repo.join("manifest.toml"),
        format!(
            r#"
        [[components]]
        name = "config"
        packages = ["native-example", "aur:aur-example", "local:recipe"]
        post_install = ["test -f '{}/config/test/settings' && printf 'install\\n' >> '{}' "]
        post_deploy = ["printf 'deploy\\n' >> '{}' "]
        [[components.files]]
        src = "settings"
        dest = "$XDG_CONFIG_HOME/test/settings"
    "#,
            root.path().display(),
            hooks.display(),
            hooks.display()
        ),
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_waysync"))
            .args(args)
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", &tools)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    invoke(&["install", "config"]);
    assert_eq!(fs::read_to_string(&hooks).unwrap(), "install\ndeploy\n");
    fs::write(&calls, "").unwrap();
    invoke(&["sync"]);
    let audit = fs::read_to_string(&calls).unwrap();
    assert!(audit.contains("pacman"));
    assert!(
        !audit.contains("paru") && !audit.contains("sudo"),
        "Installed packages must only be audited, not reinstalled"
    );
    fs::write(&calls, "").unwrap();
    assert_eq!(
        fs::read_to_string(&hooks).unwrap(),
        "install\ndeploy\ndeploy\n"
    );
    invoke(&["deploy", "--no-hooks"]);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "");
    assert_eq!(
        fs::read_to_string(&hooks).unwrap(),
        "install\ndeploy\ndeploy\n"
    );
    invoke(&["deploy"]);
    assert_eq!(
        fs::read_to_string(&hooks).unwrap(),
        "install\ndeploy\ndeploy\ndeploy\n"
    );
    let state_path = root.path().join("state/waysync/state.toml");
    let state_before = fs::read(&state_path).unwrap();
    fs::write(repo.join("dotfiles/settings"), "new source").unwrap();
    invoke(&["reinstall", "config"]);
    let log = fs::read_to_string(calls).unwrap();
    assert!(log.contains("-S --rebuild --noconfirm native-example"));
    assert!(log.contains("-S --aur --rebuild --noconfirm aur-example"));
    assert!(log.contains("-Ui --rebuild --noconfirm"));
    assert_eq!(
        fs::read_to_string(root.path().join("config/test/settings")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(&hooks).unwrap(),
        "install\ndeploy\ndeploy\ndeploy\n"
    );
    assert_eq!(fs::read(&state_path).unwrap(), state_before);
    invoke(&["uninstall", "config"]);
    assert!(!root.path().join("config/test/settings").exists());
    let state: toml::Value = fs::read_to_string(&state_path).unwrap().parse().unwrap();
    assert_eq!(state["components"]["config"].as_bool(), Some(false));
    assert!(state["installed"].as_array().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(&hooks).unwrap(),
        "install\ndeploy\ndeploy\ndeploy\n"
    );
}

#[test]
fn sync_restores_manually_removed_native_aur_and_split_packages_but_deploy_does_not() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("data/waysync/repo");
    let tools = root.path().join("tools");
    let calls = root.path().join("calls");
    let database = root.path().join("installed");
    let fail_query = root.path().join("fail-query");
    fs::create_dir_all(repo.join("dotfiles")).unwrap();
    fs::create_dir_all(repo.join("packages/recipe")).unwrap();
    fs::create_dir_all(&tools).unwrap();
    std::os::unix::fs::symlink("/bin/sh", tools.join("sh")).unwrap();
    let complete = "native-example\naur-example\nsplit-a\nsplit-b\n";
    fs::write(&database, complete).unwrap();
    let scripts = [
        (
            "pacman",
            format!(
                r#"#!/bin/sh
printf 'pacman %s\n' "$*" >> '{}'
if [ -f '{}' ]; then printf 'database query failed\n' >&2; exit 1; fi
case "$*" in
  '-T '*) printf '%s\n' "$2"; exit 127;;
  '-Qq') /bin/cat '{}';;
  '-Qq '*) while IFS= read -r name; do [ "$name" = "$2" ] && exit 0; done < '{}'; exit 1;;
  '-Q '*) printf '%s 0-1\n' "$2";;
esac
"#,
                calls.display(),
                fail_query.display(),
                database.display(),
                database.display()
            ),
        ),
        (
            "paru",
            format!(
                r#"#!/bin/sh
printf 'paru %s\n' "$*" >> '{}'
[ "$1" = --version ] && exit 0
printf 'native-example\naur-example\nsplit-a\nsplit-b\n' > '{}'
"#,
                calls.display(),
                database.display()
            ),
        ),
        (
            "makepkg",
            format!(
                r#"#!/bin/sh
printf 'makepkg %s\n' "$*" >> '{}'
printf 'pkgname = split-a\npkgname = split-b\npkgver = 1\npkgrel = 1\n'
"#,
                calls.display()
            ),
        ),
        (
            "sudo",
            format!(
                "#!/bin/sh\nprintf 'sudo %s\\n' \"$*\" >> '{}'\nexit 0\n",
                calls.display()
            ),
        ),
    ];
    for (name, script) in scripts {
        let path = tools.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        repo.join("packages/recipe/PKGBUILD"),
        "pkgname=('split-a' 'split-b')\npkgver=1\npkgrel=1\n",
    )
    .unwrap();
    fs::write(repo.join("dotfiles/settings"), "initial").unwrap();
    fs::write(repo.join("manifest.toml"), "[[components]]\nname='config'\nask=true\npackages=['native-example','aur:aur-example','local:recipe']\npost_install=['printf install >> hooks']\npost_deploy=['printf deploy >> hooks']\n[[components.files]]\nsrc='settings'\ndest='$XDG_CONFIG_HOME/test/settings'\n").unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_waysync"))
            .args(args)
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", &tools)
            .output()
            .unwrap()
    };
    let success = |args: &[&str]| {
        let output = invoke(args);
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    success(&["install", "config"]);
    let hook_baseline = fs::read(repo.join("hooks")).unwrap();
    fs::write(repo.join("dotfiles/settings"), "new config").unwrap();
    fs::write(&calls, "").unwrap();
    fs::write(&database, "aur-example\nsplit-a\n").unwrap();
    // Optional file updates default to skipped without a terminal; repairs still happen.
    success(&["sync"]);
    let log = fs::read_to_string(&calls).unwrap();
    assert!(log.contains("paru -S --needed --noconfirm native-example"));
    assert!(
        log.contains("paru -Ui --noconfirm"),
        "A missing split output must rebuild its local recipe: {log}"
    );
    assert!(
        !log.contains("--aur"),
        "The installed AUR package should not be reinstalled"
    );
    assert_eq!(fs::read_to_string(&database).unwrap(), complete);
    assert_eq!(
        fs::read(repo.join("hooks")).unwrap(),
        hook_baseline,
        "Package repair must not rerun installation or skipped deployment hooks"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("config/test/settings")).unwrap(),
        "initial"
    );

    fs::write(&database, "native-example\nsplit-a\nsplit-b\n").unwrap();
    fs::write(&calls, "").unwrap();
    success(&["sync"]);
    let log = fs::read_to_string(&calls).unwrap();
    assert!(log.contains("paru -S --aur --needed --noconfirm aur-example"));
    assert!(!log.contains("paru -Ui") && !log.contains("--noconfirm native-example"));

    // A failed query stops safely; it must not look like every package was removed.
    fs::write(&fail_query, "fail").unwrap();
    fs::write(&calls, "").unwrap();
    assert!(!invoke(&["sync"]).status.success());
    let log = fs::read_to_string(&calls).unwrap();
    assert!(!log.contains("paru") && !log.contains("sudo"));
    fs::remove_file(&fail_query).unwrap();

    fs::write(&database, "").unwrap();
    fs::write(&calls, "").unwrap();
    success(&["deploy", "--no-hooks", "config"]);
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "",
        "Deploy must never inspect or repair packages"
    );
    assert_eq!(fs::read_to_string(&database).unwrap(), "");
    assert_eq!(
        fs::read_to_string(root.path().join("config/test/settings")).unwrap(),
        "new config"
    );

    success(&["uninstall", "config"]);
    fs::write(&calls, "").unwrap();
    success(&["sync"]);
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "",
        "A component removed through Waysync must stay removed"
    );
    assert!(!root.path().join("config/test/settings").exists());
}

#[test]
fn capture_cli_remembers_authoring_checkout_and_leaves_git_and_deployment_state_separate() {
    let root = tempfile::tempdir().unwrap();
    let origin = root.path().join("authoring");
    fs::create_dir_all(origin.join("dotfiles")).unwrap();
    fs::create_dir_all(origin.join("components")).unwrap();
    fs::write(
        origin.join("manifest.toml"),
        "include=['components/editor.toml']\n",
    )
    .unwrap();
    fs::write(origin.join("components/editor.toml"), "[[components]]\nname='config'\nask=true\n[[components.files]]\nsrc='settings'\ndest='$XDG_CONFIG_HOME/test/settings'\n").unwrap();
    fs::write(origin.join("dotfiles/settings"), "original").unwrap();
    git(&origin, &["init", "-b", "main"]);
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-m", "initial"]);
    assert!(run(root.path(), &["init", origin.to_str().unwrap()])
        .status
        .success());
    assert!(run(root.path(), &["install", "config"]).status.success());
    fs::write(root.path().join("config/test/settings"), "edited locally").unwrap();
    let state_path = root.path().join("state/waysync/state.toml");
    let before = fs::read(&state_path).unwrap();
    let output = run(
        root.path(),
        &["capture", "--repo", origin.to_str().unwrap(), "--dry-run"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("edited locally"));
    assert_eq!(fs::read(&state_path).unwrap(), before);
    assert_eq!(
        fs::read_to_string(origin.join("dotfiles/settings")).unwrap(),
        "original"
    );
    let output = run(
        root.path(),
        &["capture", "--repo", origin.to_str().unwrap(), "--yes"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(origin.join("dotfiles/settings")).unwrap(),
        "edited locally"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("data/waysync/repo/dotfiles/settings")).unwrap(),
        "original"
    );
    let output = Command::new("git")
        .args(["diff", "--name-only"])
        .current_dir(&origin)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "dotfiles/settings"
    );
    let output = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(&origin)
        .output()
        .unwrap();
    assert!(output.stdout.is_empty(), "Capture must not stage changes");
    let unchanged = run(root.path(), &["capture", "--yes"]);
    assert!(unchanged.status.success());
    assert_eq!(String::from_utf8_lossy(&unchanged.stdout), "No changes.\n");
}

#[test]
fn module_removal_preserves_direct_and_transitive_dependencies_and_batches_joint_removals() {
    use std::os::unix::fs::PermissionsExt;
    for (remove_both, only_required) in [(false, false), (true, false), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("data/waysync/repo");
        let tools = root.path().join("tools");
        let calls = root.path().join("calls");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&tools).unwrap();
        fs::create_dir_all(root.path().join("state/waysync")).unwrap();
        let packages = if only_required {
            "['provider','shared']"
        } else {
            "['provider','shared','unused']"
        };
        fs::write(repo.join("manifest.toml"), format!("[[components]]\nname='core'\npackages={packages}\n[[components]]\nname='dependent'\npackages=['app','shared']\n")).unwrap();
        fs::write(
            root.path().join("state/waysync/state.toml"),
            "installed=['core','dependent']\n[components]\ncore=true\ndependent=true\n",
        )
        .unwrap();
        let pacman = format!(
            r#"#!/bin/sh
printf 'pacman %s\n' "$*" >> '{}'
if [ "$1" = -Qq ]; then printf 'provider\nshared\nunused\napp\n'; exit 0; fi
if [ "$1" = -R ]; then
    while [ "$1" != -- ]; do shift; done
    shift
    removing_app=false
    for name in "$@"; do [ "$name" = app ] && removing_app=true; done
    for name in "$@"; do
        if [ "$name" != provider ] || [ "$removing_app" = true ]; then printf '%s\n' "$name"; fi
    done
    exit 0
fi
exit 1
"#,
            calls.display()
        );
        let sudo = format!(
            "#!/bin/sh\nprintf 'sudo %s\\n' \"$*\" >> '{}'\nexit 0\n",
            calls.display()
        );
        for (name, script) in [("pacman", pacman), ("sudo", sudo)] {
            let path = tools.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let arguments = if remove_both {
            vec!["uninstall", "core", "dependent"]
        } else {
            vec!["uninstall", "core"]
        };
        let output = Command::new(env!("CARGO_BIN_EXE_waysync"))
            .args(arguments)
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", &tools)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let log = fs::read_to_string(calls).unwrap();
        let transactions: Vec<_> = log
            .lines()
            .filter(|line| line.starts_with("sudo pacman"))
            .collect();
        assert_eq!(
            transactions.len(),
            if only_required { 0 } else { 1 },
            "All removed modules must use at most one package transaction"
        );
        let transaction = transactions.first().copied().unwrap_or("");
        if !only_required {
            assert!(transaction.contains("-R --unneeded --noconfirm --"));
        }
        assert!(!log.contains("--cascade") && !log.contains("--nodeps") && !log.contains("-Rns"));
        if remove_both {
            for name in ["app", "provider", "shared", "unused"] {
                assert!(transaction.split_whitespace().any(|part| part == name));
            }
        } else {
            if !only_required {
                assert!(transaction.ends_with("-- unused"));
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("Keeping provider: required by another installed package"));
            assert!(stdout.contains("Keeping shared: claimed by a retained component"));
        }
        let saved: toml::Value = fs::read_to_string(root.path().join("state/waysync/state.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(saved["components"]["core"].as_bool(), Some(false));
        assert_eq!(
            saved["components"]["dependent"].as_bool(),
            Some(!remove_both)
        );
    }
}

#[test]
fn check_validates_unknown_fields_inside_manifest_fragments() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("authoring");
    fs::create_dir_all(repo.join("components")).unwrap();
    fs::write(
        repo.join("manifest.toml"),
        "include=['components/test.toml']\n",
    )
    .unwrap();
    fs::write(
        repo.join("components/test.toml"),
        "[[components]]\nname='config'\ndefualt=true\n",
    )
    .unwrap();
    let output = run(root.path(), &["check", "--repo", repo.to_str().unwrap()]);
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("components/test.toml") && stdout.contains("defualt"));
    assert!(!root.path().join("state").exists());
}

#[test]
fn providers_and_stable_git_variants_are_accepted_and_failed_packages_do_not_block_other_modules() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("data/waysync/repo");
    let tools = root.path().join("tools");
    let log = root.path().join("calls");
    fs::create_dir_all(repo.join("dotfiles")).unwrap();
    fs::create_dir_all(&tools).unwrap();
    fs::create_dir_all(root.path().join("state/waysync")).unwrap();
    std::os::unix::fs::symlink("/bin/sh", tools.join("sh")).unwrap();
    for (name, body) in [
        ("pacman", "if [ \"$1\" = -Qq ]; then printf 'astra-atlas\\npapirus-folders-git\\ncustom-provider\\n'; exit 0; fi\nif [ \"$1\" = -T ]; then [ \"$2\" = provider-alias ] && exit 0; printf '%s\\n' \"$2\"; exit 127; fi\nexit 1\n"),
        ("paru", "[ \"$1\" = --version ] && exit 0\ncase \"$*\" in *broken-package*) exit 1;; esac\nexit 0\n"),
        ("sudo", "exit 0\n"),
    ] {
        let path = tools.join(name);
        fs::write(&path, format!("#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> '{}'\n{body}", log.display())).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        repo.join("manifest.toml"),
        r#"
        [[components]]
        name='variants'
        packages=['astra-atlas-git','papirus-folders','provider-alias']
        [[components]]
        name='broken'
        packages=['broken-package']
        post_deploy=['printf bad >> hooks']
        [[components.files]]
        src='config'
        dest='$XDG_CONFIG_HOME/broken'
        [[components]]
        name='good'
        packages=['good-package']
        [[components.files]]
        src='config'
        dest='$XDG_CONFIG_HOME/good'
        [[components]]
        name='healthy'
        [[components.files]]
        src='config'
        dest='$XDG_CONFIG_HOME/healthy'
        [[components]]
        name='fresh'
        packages=['broken-package','good-package']
        post_install=['printf bad >> hooks']
        [[components.files]]
        src='config'
        dest='$XDG_CONFIG_HOME/fresh'
    "#,
    )
    .unwrap();
    fs::write(repo.join("dotfiles/config"), "working").unwrap();
    let state_path = root.path().join("state/waysync/state.toml");
    fs::write(&state_path, "installed=['variants','broken','good','healthy']\n[components]\nvariants=true\nbroken=true\ngood=true\nhealthy=true\n").unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_waysync"))
            .args(args)
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", &tools)
            .output()
            .unwrap()
    };
    let output = invoke(&["sync"]);
    assert!(
        !output.status.success(),
        "Failures must still be reported at the end"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Completed remaining work"));
    let calls = fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("--noconfirm astra-atlas-git")
            && !calls.contains("--noconfirm papirus-folders")
            && !calls.contains("--noconfirm provider-alias")
    );
    assert!(calls.contains("--noconfirm good-package"));
    for name in ["good", "healthy"] {
        assert_eq!(
            fs::read_to_string(root.path().join("config").join(name)).unwrap(),
            "working"
        );
    }
    assert!(!root.path().join("config/broken").exists());
    assert!(!repo.join("hooks").exists());

    let mut state: toml::Value = fs::read_to_string(&state_path).unwrap().parse().unwrap();
    state["installed"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v.as_str() != Some("variants"));
    fs::write(&state_path, toml::to_string(&state).unwrap()).unwrap();
    fs::write(&log, "").unwrap();
    assert!(invoke(&["install", "variants"]).status.success());
    assert!(
        !fs::read_to_string(&log).unwrap().contains("paru"),
        "First installation must also respect existing variants/providers"
    );

    fs::write(repo.join("dotfiles/config"), "updated healthy config").unwrap();
    fs::write(&log, "").unwrap();
    assert!(!invoke(&["install", "fresh", "healthy"]).status.success());
    let calls = fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("--noconfirm broken-package") && calls.contains("--noconfirm good-package")
    );
    assert_eq!(
        fs::read_to_string(root.path().join("config/healthy")).unwrap(),
        "updated healthy config"
    );
    assert!(!root.path().join("config/fresh").exists());
    let state: toml::Value = fs::read_to_string(state_path).unwrap().parse().unwrap();
    assert_eq!(state["components"]["fresh"].as_bool(), Some(true));
    assert!(!state["installed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("fresh")));
    assert!(!repo.join("hooks").exists());
}

#[test]
fn capture_uses_sudo_read_without_unprivileged_stat_of_protected_parent() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("data/waysync/repo");
    let author = root.path().join("author");
    let tools = root.path().join("tools");
    let protected = root.path().join("protected");
    for dir in [&repo, &author] {
        fs::create_dir_all(dir.join("dotfiles")).unwrap();
    }
    fs::create_dir_all(&tools).unwrap();
    fs::create_dir_all(&protected).unwrap();
    fs::create_dir_all(root.path().join("state/waysync")).unwrap();
    let dest = protected.join("secret");
    fs::write(&dest, "deployed edit").unwrap();
    let manifest = format!("[[components]]\nname='secret'\nask=true\n[[components.files]]\nsrc='secret'\ndest='{}'\nsudo=true\n", dest.display());
    for dir in [&repo, &author] {
        fs::write(dir.join("manifest.toml"), &manifest).unwrap();
        fs::write(dir.join("dotfiles/secret"), "original").unwrap();
    }
    git(&author, &["init", "-b", "main"]);
    std::os::unix::fs::symlink("/usr/bin/git", tools.join("git")).unwrap();
    let sudo = tools.join("sudo");
    fs::write(
        &sudo,
        "#!/bin/sh\n[ \"$1\" = cat ] || exit 1\nprintf 'deployed edit'\n",
    )
    .unwrap();
    fs::set_permissions(sudo, fs::Permissions::from_mode(0o755)).unwrap();
    let baseline = hex::encode(Sha256::digest(b"original"));
    fs::write(root.path().join("state/waysync/state.toml"), format!("installed=['secret']\n[components]\nsecret=true\n[hashes]\n'{}'='{}'\n[deployed]\nsecret=['{}']\n[sources]\n'{}'='dotfiles/secret'\n", dest.display(), baseline, dest.display(), dest.display())).unwrap();
    fs::set_permissions(&protected, fs::Permissions::from_mode(0o000)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_waysync"))
        .args(["capture", "--repo", author.to_str().unwrap(), "--yes"])
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("PATH", &tools)
        .output()
        .unwrap();
    fs::set_permissions(&protected, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(author.join("dotfiles/secret")).unwrap(),
        "deployed edit"
    );
}

#[test]
fn privileged_copy_uses_real_mktemp_correctly_and_reports_its_stderr() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("data/waysync/repo");
    let tools = root.path().join("tools");
    let fail = root.path().join("fail-mktemp");
    fs::create_dir_all(repo.join("dotfiles")).unwrap();
    fs::create_dir_all(&tools).unwrap();
    fs::write(repo.join("dotfiles/script"), "#!/bin/sh\nprintf hello\\n\n").unwrap();
    fs::set_permissions(
        repo.join("dotfiles/script"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::write(repo.join("manifest.toml"), "[[components]]\nname='privileged'\n[[components.files]]\nsrc='script'\ndest='$XDG_CONFIG_HOME/folder with spaces/script'\nsudo=true\n").unwrap();
    let sudo = tools.join("sudo");
    // No privilege escalation in this test. Execute real coreutils in the temp tree.
    fs::write(
        &sudo,
        format!(
            r#"#!/bin/sh
[ "$1" = -v ] && exit 0
cmd="$1"; shift
if [ "$cmd" = mktemp ] && [ -f '{}' ]; then printf 'test allocation denied\n' >&2; exit 1; fi
exec "/usr/bin/$cmd" "$@"
"#,
            fail.display()
        ),
    )
    .unwrap();
    fs::set_permissions(sudo, fs::Permissions::from_mode(0o755)).unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_waysync"))
            .args(["install", "privileged"])
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", &tools)
            .output()
            .unwrap()
    };
    let output = invoke();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let dest = root.path().join("config/folder with spaces/script");
    assert_eq!(
        fs::read(&dest).unwrap(),
        fs::read(repo.join("dotfiles/script")).unwrap()
    );
    assert_eq!(
        fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_dir(dest.parent().unwrap()).unwrap().count(),
        1,
        "Temporary files must not remain"
    );
    fs::write(&fail, "fail").unwrap();
    fs::write(repo.join("dotfiles/script"), "updated script").unwrap();
    let output = invoke();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("test allocation denied"));
    assert_ne!(fs::read_to_string(dest).unwrap(), "updated script");
}
