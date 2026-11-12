#!/usr/bin/env bash
set -euo pipefail

WAYSYNC_REPO_URL="${WAYSYNC_REPO_URL:-https://github.com/GwynnN7/waysync.git}"
WAYSYNC_BRANCH="${WAYSYNC_BRANCH:-main}"
DOTFILES_REPO_URL="${1:-}"

BOLD="\033[1m"
CYAN="\033[36m"
GREEN="\033[32m"
RED="\033[31m"
RESET="\033[0m"

info() {
    printf "${BOLD}${CYAN}==>${RESET}${BOLD}%s${RESET}\n" "$*"
}

ok() {
    printf "${BOLD}${GREEN}✓${RESET} %s\n" "$*"
}

error() {
    printf "${BOLD}${RED}✗ Error:${RESET} %s\n" "$*" >&2
    exit 1
}

if [[ ! -f /etc/arch-release ]] && ! command -v pacman >/dev/null 2>&1; then
    error "waysync requires an Arch-based Linux distribution"
fi

MISSING_PKGS=()

command -v git >/dev/null 2>&1 || MISSING_PKGS+=("git")
command -v cc >/dev/null 2>&1 || MISSING_PKGS+=("base-devel")
command -v cargo >/dev/null 2>&1 || MISSING_PKGS+=("rust")

if (( ${#MISSING_PKGS[@]} > 0 )); then
    info "Installing required build dependencies: ${MISSING_PKGS[*]}"
    sudo pacman -S --needed --noconfirm "${MISSING_PKGS[@]}" </dev/tty
fi

TMP_DIR="$(mktemp -d -t waysync-install-XXXXXX)"

cleanup() {
    rm -rf "$TMP_DIR"
}

trap cleanup EXIT

info "Cloning waysync from ${WAYSYNC_REPO_URL} (branch:${WAYSYNC_BRANCH})..."
git clone --depth 1 --branch "$WAYSYNC_BRANCH" "$WAYSYNC_REPO_URL" "$TMP_DIR/waysync"

info "Building waysync (release profile)..."
cargo build --release --locked --manifest-path "$TMP_DIR/waysync/Cargo.toml"

info "Installing waysync to /usr/local/bin/waysync..."
sudo install -Dm755 "$TMP_DIR/waysync/target/release/waysync" /usr/local/bin/waysync </dev/tty

ok "waysync installed to /usr/local/bin/waysync"

if [[ -n "$DOTFILES_REPO_URL" ]]; then
    info "Initializing dotfiles repository: ${DOTFILES_REPO_URL}"
    waysync init "$DOTFILES_REPO_URL" </dev/tty
else
    printf "\n"
    ok "Installation complete!"
    printf "  Run ${CYAN}waysync init <dotfiles-repo-url>${RESET} to set up your dotfiles.\n"
    printf "  Then run ${CYAN}waysync${RESET} to manage your system.\n\n"
fi