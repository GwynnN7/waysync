# Waysync

**Waysync** is a declarative dotfile and package manager for Arch-based Linux distributions. It provides a full-screen interactive TUI to manage system packages (native Arch packages, AUR packages via `paru`, and local `PKGBUILD`s), deploy dotfiles with SHA256 conflict detection, and switch between desktop environments or shells (such as `caelestia`, `noctalia`, GNOME, or KDE) using per-file overrides.

## Quick Installation

Install `waysync` directly via `curl | bash`:

```bash
curl -fsSL https://raw.githubusercontent.com/GwynnN7/waysync/main/install.sh | bash
```

Install `waysync` and initialize your dotfiles repository in a single command:

```bash
curl -fsSL https://raw.githubusercontent.com/GwynnN7/waysync/main/install.sh | bash -s -- https://github.com/GwynnN7/dotfiles.git
```

## CLI Usage

| Command | Description |
| :--- | :--- |
| `waysync init <url>` | Clone your dotfiles repository into `~/.local/share/waysync/repo` and initialize state. |
| `waysync` | Launch the full-screen interactive TUI menu. |
| `waysync env` | Switch the active environment (or select `none` for global-only) and configure environment-bound components. |
| `waysync update` | Pull the latest changes from your dotfiles repository and sync enabled components. |
| `waysync status` | Display the current repository, active environment, and deployed component status. |

When launched without a command, Waysync refreshes the dotfiles repository before opening the menu. If the remote cannot be reached, it opens the menu using the local copy instead.

### Interactive TUI Controls

**Categories View:**
* `Up` / `Down` (or `j` / `k`): Navigate items.
* `Space`: Toggle an entire category on or off (if partially enabled, pressing `Space` enables all components in that category).
* `Enter` (or `Right` / `l`): Open a category to toggle individual components, or trigger `Save & Apply`.
* `Esc` / `q`: Cancel or exit.

**Component View (Inside a Category):**
* `Up` / `Down` (or `j` / `k`): Navigate components.
* `Space`: Toggle the selected component.
* `a`: Toggle all components in the current category.
* `Enter` / `Esc` / `Left` / `h` / `q`: Return to the Categories view.

## Dotfiles Repository Structure

waysync expects your dotfiles repository to contain `manifest.toml` at the root alongside `dotfiles/`, and optionally `overrides/` and `packages/`:

```text
my-dotfiles-repo/
├── manifest.toml          # Declarative configuration of environments & components
├── dotfiles/              # Base/global dotfiles (files and directories)
├── overrides/             # Optional environment-specific file overrides
│   └── <environment>/
└── packages/              # Optional local PKGBUILDs (referenced as "local:<name>")
    ├── shell/
    │   └── PKGBUILD
    └── kdeconnect/
        └── PKGBUILD
```

### How Overrides Work (`dotfiles/` vs `overrides/<env>/`)

When an environment (e.g., `caelestia` or `noctalia`) is active, waysync resolves every `[[components.files]]` entry as follows:

1. **Single File (`src = "starship.toml"`):** waysync checks if `overrides/<env>/starship.toml` exists. If found, it deploys that override; otherwise, it falls back to `dotfiles/starship.toml`.
2. **Directory (`src = "hypr"`):** waysync scans `dotfiles/hypr/` first and then merges `overrides/<env>/hypr/` over it. Files are selected by relative path—so `overrides/noctalia/hypr/hyprland.conf` replaces `dotfiles/hypr/hyprland.conf`, while base-only and override-only files are both deployed.
3. **Switching Environments:** Running `waysync env` removes only old-environment packages that are not claimed by another enabled component, without recursively removing their dependencies; it also undeploys old files, installs the new environment's components, and updates only enabled global components whose effective files or environment-scoped hooks change. Unrelated global components are not re-deployed. An affected component with `ask = true` still requires confirmation before it is updated.

### Per-file Environment Scope

Use `environment` on an individual `[[components.files]]` entry when a component has both shared files and files that should exist only in one environment. Entries without it deploy in every environment; a tagged entry deploys only while its named environment is active. On an environment change, Waysync updates affected enabled global components and removes files that no longer apply.

This scope applies only to a file entry. Packages and hooks remain component-level; keep those in an environment-specific component when they must not run or install globally.

> **Note on External Git Repositories (`repo:<user>/<repo>`):**
> Entries using `src = "repo:<user>/<repo>"` are cloned and updated directly from GitHub into their target `dest` directories. They do not need to be stored inside `dotfiles/`.

## Manifest Schema Reference (`manifest.toml`)

```toml
# Optional: Define environments/shells. Omit if you only use global components.
[environments]
available = ["caelestia", "noctalia"]

[[components]]
name = "hypr"                          # Unique component identifier
category = "wm"                        # Menu category (defaults to "general")
default = true                         # Checked by default on first run (default: false)
ask = true                             # Ask before updating an installed component (default: false)
environment = "caelestia"              # Optional: bind component to a specific environment
packages = [
    "hyprland",                        # Native Arch or AUR package (installed via paru)
    "local:my-custom-pkg",             # Local PKGBUILD in packages/my-custom-pkg/PKGBUILD
]
post_install = [                       # Shell commands run ONLY when newly enabled
    "hyprctl reload >/dev/null",
]
post_deploy = [                        # Shell commands run after every dotfile deployment
    "hyprctl reload >/dev/null",
]

[[components.files]]
src = "hypr"                           # Path in dotfiles/ (or overrides/<env>/), OR "repo:user/repo"
dest = "$XDG_CONFIG_HOME/hypr"         # Destination path (supports env vars)
sudo = false                           # Copy/remove using sudo (default: false)
check = true                           # Prompt with diff if modified locally (default: true)
environment = "caelestia"              # Optional: deploy this file only in this environment
```

### Supported Environment Variables in `dest`

waysync expands any environment variable set in your current session and provides built-in fallbacks for standard XDG variables:

* `$HOME` -> `/home/<user>`
* `$XDG_CONFIG_HOME` -> `$HOME/.config`
* `$XDG_DATA_HOME` -> `$HOME/.local/share`
* `$XDG_STATE_HOME` -> `$HOME/.local/state`
* `$XDG_CACHE_HOME` -> `$HOME/.cache`
* `$XDG_BIN_HOME` -> `$HOME/.local/bin`
* `$XDG_CONFIG_SERVICE` -> `$HOME/.config/systemd/user`
