# Waysync

Waysync manages dotfiles and packages on Arch-based Linux systems. Choose the components you want, select an environment and an independent **Desktop / Laptop** device profile, then apply the configuration from one Git repository.

- Merge shared, environment, device, and device-plus-environment files.
- Install native, AUR (`aur:`), and local `PKGBUILD` (`local:`) packages.
- Review local file conflicts with a diff before overwriting them.
- Capture edits from deployed `ask = true` components back into your authoring checkout.
- Restore manually removed managed packages on the next sync.
- Check a repository without installing packages, running hooks, or changing files.
- Use a keyboard-driven terminal UI or explicit CLI commands.

## Install

Requires an Arch-based distribution, Git, Rust 1.89+, and the `base-devel` tools for package builds. Waysync bootstraps `paru-bin` from the AUR when package installation first needs it. Run Waysync as your normal user; privileged steps request `sudo`.

From this checkout:

```bash
cargo build --release --locked
sudo install -Dm755 target/release/waysync /usr/local/bin/waysync
waysync init https://github.com/GwynnN7/dotfiles.git
waysync check --all
waysync
```

Or use the installer, optionally passing your dotfiles repository:

```bash
curl -fsSL https://raw.githubusercontent.com/GwynnN7/waysync/main/install.sh | bash
# Initialize at the same time:
curl -fsSL https://raw.githubusercontent.com/GwynnN7/waysync/main/install.sh | bash -s -- https://github.com/GwynnN7/dotfiles.git
```

`init` clones and validates the manifest. It offers environment and device choices in a terminal, but does not install components or deploy files. Without a terminal it initializes with both choices unset.

Only use dotfiles repositories you trust: installing packages evaluates their build recipes, and manifest hooks are shell commands.

## Everyday use

| Command | Behavior |
| --- | --- |
| `waysync` | Open the menu using the local checkout. |
| `waysync init <url>` | Initialize a managed dotfiles checkout. Local Git paths also work. |
| `waysync env` | Choose an environment, including `none`. |
| `waysync env caelestia` | Select a named environment; newly available components may prompt. |
| `waysync device` | Choose Desktop, Laptop, or `none`. |
| `waysync device laptop` | Select Laptop while keeping the environment. |
| `waysync device none` | Clear device-specific configuration. |
| `waysync deploy` | Deploy dotfiles for installed, enabled components and run `post_deploy`. No packages. |
| `waysync deploy --no-hooks` | Deploy dotfiles only; skip all hooks and package operations. |
| `waysync deploy fish` | Deploy only the named installed component(s). |
| `waysync sync` | Reconcile saved choices and actual package presence: restore missing packages, install new components, remove disabled ones, and deploy retained components. No pull or forced package refresh. |
| `waysync install fish` | Install packages and files, run setup hooks, and enable the component. |
| `waysync uninstall fish` | Remove unshared packages and unchanged managed files, then disable the component. |
| `waysync capture --repo ~/Projects/dotfiles --dry-run` | Preview deployed edits to capture into your authoring checkout. |
| `waysync capture --repo ~/Projects/dotfiles` | Review and copy edits; remember the checkout for later captures. |
| `waysync capture --yes` | Copy nonconflicting edits into the remembered checkout without prompts. |
| `waysync reinstall fish` | Force package reinstall/builds only; omit names for all enabled, installed components. |
| `waysync update` | Pull with `--ff-only` and validate the manifest. No packages, files, or hooks. |
| `waysync status` | Show profile, enabled components, and tracked file counts. |
| `waysync check` | Check all components available to the active profile, including optional ones. |
| `waysync check --all` | Check all environments with unset, Desktop, and Laptop devices. |
| `waysync check --repo ~/Projects/dotfiles --all` | Check another local checkout without initializing it. |

`waysync shell` remains an alias for `waysync env`.

Opening the menu does **not** pull or modify the checkout. Repository updates never clean build output or restore local edits automatically. Resolve Git conflicts or local changes yourself, then retry.

### Terminal UI

The home menu shows the active environment/device and four groups:

| Menu | Actions |
| --- | --- |
| Components | Manage, Install, Uninstall, Reinstall |
| Dotfiles | Deploy, Capture edits |
| Profile | Environment, Device |
| Repository | Pull changes, Check |

Status and Quit are on the home menu. `Esc` or Back returns one level; actions return to the same submenu and selection. Long component/category lists scroll to keep the selected row visible. Routine output is brief: capture shows `No changes.` or `Captured 3 files.`; diffs and errors appear when relevant.

| View | Keys |
| --- | --- |
| Any list | `↑` / `↓` or `k` / `j` to move; `Enter` to choose; `Esc` / `q` to leave. |
| Categories | `Space` toggles the category; `Enter` / `l` opens its components. |
| Components | `Space` toggles one; `a` toggles all matches; `/` filters by name; `Enter` / `Esc` / `h` goes back. |
| Status | `↑` / `↓` scrolls; `Enter` / `Esc` returns. |

**Components → Manage** starts with remembered choices, or `default` for new components. Applying the selection is a system operation, not just a preference save. New selections install packages and files; unchecked installed components are uninstalled; retained selections restore any missing packages and deploy their files without forcing upgrades or rebuilds. The confirmation names the removals.

**Components → Install** offers components Waysync has not installed. **Components → Uninstall** offers tracked installed components. Both perform real system/file changes and update Waysync's tracking. Both change the system; neither is a bookkeeping-only action.

**Components → Reinstall** is a separate action for installed components with packages. The old reinstall checkbox is gone: forcing package installation has no purpose in file deployment or uninstall. It does not deploy files, run hooks, or change component choices. **Dotfiles → Deploy** asks whether to run deployment hooks; answer No for file copying only.

### Action boundaries

“Installed” means recorded as installed by Waysync, including components with no packages. Packages installed outside Waysync are not automatically enrolled. Installing a component adopts it into management; already present packages are skipped with `--needed`.

| Action | Packages | Dotfiles | Hooks | Component choices / installation tracking |
| --- | --- | --- | --- | --- |
| Deploy dotfiles | None | Enabled, installed components | `post_deploy` only | Unchanged; file hashes/ownership updated |
| Deploy with `--no-hooks` | None | Enabled, installed components | None | Unchanged; file hashes/ownership updated |
| Manage → Apply Selection / `sync` | Install new components, restore missing packages, remove deselected ones | Selected components | `post_install` for new installs, then `post_deploy` | Updated to reflect applied selection |
| Install components | Install missing packages | Selected components | New-install setup, then deployment hooks | Enable and record installation |
| Uninstall components | Remove packages unclaimed by retained components | Remove unchanged tracked files | None | Disable and clear installation tracking |
| Reinstall | Force reinstall/rebuild | None | None | Unchanged |
| Update repository | None | None | None | Unchanged; only the Git checkout changes |
| Capture local changes | None | Copy deployed edits back into the authoring checkout | None | Remember checkout; deployment baselines and choices unchanged |

With an unchanged selection, **Manage → Apply Selection** also audits package presence, restores missing packages, and retries incomplete setup and cleanup. Use **Deploy Dotfiles** when you want to skip package checks entirely. Updating the repository is always separate. Use `waysync update` followed by `waysync deploy` for the common “get the new configs, then apply them” workflow.

For native repository packages, reinstall uses `paru -S` without `--needed`. For AUR packages it also requests `--rebuild`; `local:` recipes use `paru -Ui --rebuild` in a fresh build directory. Plain package names can resolve to either native or AUR packages. Reinstall is a repair/rebuild operation, not a full system upgrade. Waysync does not refresh the system package databases or perform `-Syu` on your behalf. If packages were manually removed outside Waysync, sync automatically restores the missing specs. Use Uninstall Components / `waysync uninstall` to remove them from management so later syncs leave them removed.

Operations run on the normal terminal so logs stay in scrollback. Errors remain visible and you can return to the menu.

## Capture deployed edits into your authoring checkout

Your managed checkout and your working Git checkout can be separate. For example, Waysync can deploy from `~/.local/share/waysync/repo` while you commit from `~/Projects/dotfiles`:

```bash
# Preview all tracked deployed file edits in components with ask = true:
waysync capture --repo ~/Projects/dotfiles --dry-run
# Review each diff and copy the edits you approve:
waysync capture --repo ~/Projects/dotfiles
# The path is remembered; later runs can omit --repo:
waysync capture
# Batch copy only changes without conflicting checkout edits:
waysync capture --yes
```

The TUI has **Capture Local Changes** with an authoring-checkout prompt. Capture uses the **component's** `ask = true`, not the file's `check` flag. It handles single files and individual files from merged directories. Each file returns to its original repository-relative source: shared edits go to `dotfiles/`, environment edits to `overrides/`, and device edits to their device layer. It never flattens an override into a shared file. Older deployments without recorded source paths use the current manifest mapping; uncertain or changed mappings are reported and skipped.

Capture compares the deployed contents and authoring contents against the last deployment hash. It skips unchanged deployed files and edits already present in the checkout. If both versions changed, interactive mode asks before replacing the checkout file; `--yes` skips such conflicts. Use `--yes --force` only when you explicitly want the deployed edit to replace conflicting checkout edits. Skipped paths produce a nonzero exit status with diagnostics; successfully copied files remain copied.

The authoring directory must be a Git checkout root with a compatible manifest and source layout. Existing source permissions are preserved. Protected deployed files marked `sudo = true` are read with sudo, including paths whose parent directories cannot be inspected without privileges. Source symlinks, ambiguous routes, and files changed during review are refused. Missing deployed files are reported, not treated as deletions. Untracked/generated files and external `repo:` checkouts are excluded. Capture copies file contents; it does not import deployed permission changes.

Nothing is staged, committed, or pushed, and no hooks or package managers run. Capture does not alter deployment hashes or the managed checkout when writing to a separate authoring checkout. Review and push normally, then refresh the managed checkout before applying again:

```bash
git -C ~/Projects/dotfiles diff
git -C ~/Projects/dotfiles add <reviewed-files>
git -C ~/Projects/dotfiles commit -m "Update configuration"
git -C ~/Projects/dotfiles push
waysync update
waysync deploy
```

Until that refresh, the managed checkout still has the old content; a deployment may correctly show a local conflict. Capture deliberately keeps the old baseline so it cannot silently overwrite your captured edits with that old version.

## Package dependencies and module removal

There is no separate `requirements` array. Keep each kind of requirement at its owning layer:

| Requirement | Declare it in |
| --- | --- |
| A program's normal runtime dependencies | The package / PKGBUILD `depends` |
| Tools used only to build or test a local package | Its `makedepends` / `checkdepends` |
| An optional integration you deliberately want installed | The component's `packages` |
| Helpers used by your custom scripts, hooks, or config | The component's `packages` |

Repository packages and local PKGBUILDs already describe package dependencies. Pacman/paru resolve those relationships; duplicating them in a Waysync field would create a second dependency graph to maintain. Optional dependencies are suggestions, not automatically selected software, so explicitly list any you rely on. A helper may appear in more than one component: each declaration is a retained claim, not a reason to install it twice.

On uninstall, Waysync first protects direct package claims from retained modules. It then asks pacman/libalpm for a dependency-safe removal plan using `-R --unneeded --print`, and executes only that subset with `-R --unneeded`. This also protects transitive, versioned, and provided dependencies of installed packages outside Waysync. If every candidate is still needed, the module is disabled and its unchanged managed files are removed while those packages stay installed; this is successful cleanup, not a failed uninstall. All modules being removed together share one package transaction, so their ordering does not strand dependencies that could be removed together.

No cascade, dependency-check bypass, recursive dependency removal, or automatic orphan purge is used. Chosen optional helpers and script dependencies still need explicit component package claims: pacman cannot infer dependencies from shell commands or dotfile contents. Packages retained for other software may later become ordinary orphans; Waysync leaves any subsequent cleanup to you.

References: [PKGBUILD dependency fields](https://man.archlinux.org/man/PKGBUILD.5.en.html) and [pacman removal options](https://man.archlinux.org/man/pacman.8.en).

## Missing package reconciliation

`sync`, **Manage → Apply Selection**, explicit component installation, and profile changes audit packages for the installed components they reconcile. Waysync reads pacman's installed package list and resolves `local:` recipes in temporary directories so split-package outputs are checked by their actual names. A missing output rebuilds/reinstalls that recipe. Already present package specs are left alone; package database query errors stop the operation rather than triggering a mass reinstall.

Installed providers satisfy requirements through pacman's dependency checks. For this personal setup, unversioned stable/Git counterparts are also accepted in either direction (for example `astra-atlas` / `astra-atlas-git` and `papirus-folders` / `papirus-folders-git`). Sync and first installation preserve the installed choice rather than trying to replace it. Explicit version requirements still use pacman's dependency test; a name ending in `-git` is otherwise a preference, not a demand to replace a working stable installation. Explicit **Reinstall** continues to target the manifest's chosen specs.

A failed package build/install is reported and skipped so the remaining packages/components can run. Interactive failure choices default to Skip, with Retry available. Noninteractive failures are recorded without waiting for input. A failed component is not recorded as successfully installed; its selection remains enabled for a later sync retry. For an already managed component with a failed missing-package repair, its files and hooks are deferred until the repair succeeds. The final summary lists failures and exits nonzero after remaining work has completed. Package database/query errors and invalid configuration still stop before making unsafe assumptions.

Restoration does not rerun `post_install` for an already managed component. Its normal deployment phase still runs `post_deploy`, unless that optional file update is skipped. `ask = true` controls file-update approval and capture eligibility; it does not exempt saved packages from restoration. Without a terminal, optional file updates are skipped while required package restoration still runs.

Remove a component through Waysync to stop managing its packages. Deleting packages with pacman/paru alone leaves the saved selection enabled, so sync restores them. **Deploy Dotfiles**, including `waysync deploy` and `--no-hooks`, never queries or restores packages. Capture, repository refresh, checks, and status also keep their separate scopes.

## Repository layout and device overrides

Use one branch for both machines. Environments and devices describe independent choices; branches would duplicate shared configuration and multiply combinations.

```text
my-dotfiles/
├── manifest.toml
├── dotfiles/                               # Shared files
├── overrides/
│   └── caelestia/                          # Existing environment overrides
├── devices/
│   ├── desktop/
│   │   ├── dotfiles/                       # Desktop overrides, any environment
│   │   └── overrides/caelestia/             # Desktop + Caelestia overrides
│   └── laptop/
│       ├── dotfiles/                       # Laptop overrides, any environment
│       └── overrides/caelestia/             # Laptop + Caelestia overrides
└── packages/
    └── my-package/PKGBUILD
```

For each `src`, the last existing layer wins:

| Priority | Source root |
| --- | --- |
| 1 — base | `dotfiles/` |
| 2 — environment | `overrides/<environment>/` |
| 3 — device | `devices/<device>/dotfiles/` |
| 4 — device + environment | `devices/<device>/overrides/<environment>/` |

Directories merge **by relative file path**, in the same order. An override replaces a whole file, not individual TOML/JSON/Lua settings. Base-only and override-only files are both included. Unset axes omit their layers. Existing environment-only repositories work without adding any device files.

For example, `src = "hypr"` and `dest = "$XDG_CONFIG_HOME/hypr"` can take shared keybindings from `dotfiles/hypr/`, shell bindings from `overrides/caelestia/hypr/`, and monitor settings from `devices/laptop/dotfiles/hypr/`.

See the [complete small example](examples/dotfiles/manifest.toml). Manifests can be kept in one file or split into ordered component fragments as below.

## Manifest

For a larger repository, keep environment declarations in `manifest.toml` and split modules into domain files:

```toml
# manifest.toml — include is a root key, above [environments].
include = ["components/00-shells.toml", "components/10-desktop.toml", "components/20-system.toml"]

[environments]
available = ["caelestia", "noctalia"]
```

Each fragment contains normal `[[components]]` entries. Files are loaded in the listed order; any components directly in the root come first. Includes are literal checkout-relative paths: no globs, nested includes, symlinks, or `..`. Define environments only in the root. Validation checks duplicate component names and scopes across the combined manifest and reports ignored fields in each fragment. Existing single-file manifests still work.

A single-file example:

```toml
[environments]
available = ["caelestia", "noctalia"]

[[components]]
name = "hyprland"                 # Unique identifier; keep stable for state tracking
category = "wm"                   # Default: general
default = true                   # Default: false; used only before a choice is saved
ask = false                       # Ask before updating an installed component
packages = ["hyprland", "aur:some-package", "local:my-package"]

# Hooks belong HERE, before [[components.files]].
post_install = ["printf 'First deployment completed\\n'"]
post_deploy = [
  { cmd = "hyprctl reload", environments = ["caelestia", "noctalia"] },
]

[[components.files]]
src = "hypr"
dest = "$XDG_CONFIG_HOME/hypr"
sudo = false                      # Default: false
check = true                      # Default: true; false allows unconditional overwrite

[[components]]
name = "laptop-power"
category = "hardware"
devices = ["laptop"]
default = false
packages = ["power-profiles-daemon"]

[[components.files]]
src = "power/settings.conf"
dest = "$XDG_CONFIG_HOME/power/settings.conf"
devices = ["laptop"]
environments = ["caelestia", "noctalia"]
```

Remove the example package names and supply the referenced source files before using this snippet. `waysync check` will report missing local recipes and source files; it does not check whether remote packages/repositories exist.

`environments` and `devices` are supported on components, file entries, and scoped hooks. Both scopes must match when both are present. Omitted or empty lists apply to all values of that axis. A scope on a **file** controls only that file, not its component's packages or hooks. Use a scoped **component** for device-specific packages.

Legacy `environment = "name"`, `shell = "name"`, and `[shells]` remain accepted. Environment names must be declared in `available`. Devices are built-in lowercase values: `desktop` and `laptop`; no `[devices]` section is needed.

### Hooks and external repositories

- `post_install` runs after files are deployed when a component is newly installed or reactivated. A failed installation hook is retried on the next sync.
- `post_deploy` runs after every deployment of that component.
- Retained components on a profile switch run only applicable `post_deploy` hooks. Use these for environment/device activation work; `post_install` is reserved for installing or reactivating a component. Reinstall does not invoke either hook phase.
- Hooks run through `sh -c` from the repository root, with `WAYSYNC_REPO_DIR`, `WAYSYNC_ENV`, and `WAYSYNC_DEVICE` set. Unset axes are empty strings.
- A failed hook stops that component and reports the failure. Component reconciliation continues with other components and summarizes errors afterward. For deliberately optional commands, handle the expected failure explicitly, for example `hyprctl reload || true` when running outside a session.
- `src = "repo:owner/project"` clones a GitHub repository into `dest` or pulls an existing checkout. It bypasses override layers, does not support `sudo`, and the checkout is preserved on uninstall to protect user work.

In TOML, keys after `[[components.files]]` belong to that file table until another table starts. Put component hooks before the first file table. `check` reports ignored keys and misplaced hooks and exits nonzero for any finding.

### Destination paths

Destinations must expand to absolute paths. Waysync supports `~/`, `$VAR`, `${VAR}`, and `${VAR:-fallback}`. Unset variables without fallbacks, `..`, broad roots such as `$HOME` or `/etc`, and overlap with Waysync's own files are rejected.

| Variable | Fallback |
| --- | --- |
| `$HOME` | Current home directory |
| `$XDG_CONFIG_HOME` | `$HOME/.config` |
| `$XDG_DATA_HOME` | `$HOME/.local/share` |
| `$XDG_STATE_HOME` | `$HOME/.local/state` |
| `$XDG_CACHE_HOME` | `$HOME/.cache` |
| `$XDG_BIN_HOME` | `$HOME/.local/bin` |
| `$XDG_CONFIG_SERVICE` | `$XDG_CONFIG_HOME/systemd/user` |

Dotfile sources must be regular files or directories inside the checkout. Source symlinks are rejected. Files are copied, preserving executable permissions; they are not symlinked into your home.

## Switching and recovering

A profile switch removes components that no longer apply, offers newly available components, and updates enabled retained components whose resolved files or scoped hooks change. It keeps the environment when changing device and the device when changing environment. Choices for inactive components are remembered for the next switch. Unrelated components are left alone.

Packages shared by retained components or required by other installed packages are protected. Removal uses `pacman -R --unneeded`, without recursively removing dependencies; packages still needed by other software are kept. Local builds and package metadata evaluation use disposable copies, leaving the managed checkout intact.

Conflicting local files default to **Keep local**. A pre-existing file you keep is not claimed by Waysync. On removal or when an old override disappears, unchanged tracked files are deleted; locally modified or unverified paths and external Git checkouts are preserved. Empty directories may remain. A saved `.new` file is also retained and never silently overwritten.

Files and package operations cannot form one atomic transaction. State is atomically replaced after each component and on recoverable errors, and a process lock prevents simultaneous modifying operations. Successful work remains applied if a later step fails. Fix the problem and use `waysync sync` to resume. Run `waysync update` first if you also want a pull, then explicitly deploy or sync. State records per-component progress; an abrupt kill or power loss during a component may require reviewing its files before retrying.

The managed checkout lives at `$XDG_DATA_HOME/waysync/repo`; state lives at `$XDG_STATE_HOME/waysync/state.toml`. Existing state is migrated on the next modifying operation. Back up that file before upgrading; older Waysync versions do not understand device profiles or the newer installation tracking.

`status` reports tracked state, not a live audit of installed packages or file contents. `check` validates local inputs and destination collisions without executing hooks, package recipes, package managers, or Git network requests. With `--all`, optional components are checked too, so alternative components sharing destinations may need separate scopes.

## Development

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo run --locked -- check --repo examples/dotfiles --all
```

Tests use temporary repositories and configuration directories. They cover scope combinations, override precedence, profile switches, conflicts, ownership and cleanup, failure checkpoints, legacy state, validation, menu scrolling, capture routing/conflicts, manifest fragments, dependency-safe batch removal, package restoration, and action boundaries (using fake package tools). They do not install system packages or deploy into your real home.

## AI usage

AI language models assisted with Waysync's development, including code generation, tests, and documentation.

Licensed under [GPL-3.0](LICENSE).
