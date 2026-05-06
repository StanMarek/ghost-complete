#!/usr/bin/env bash
# scripts/dev-reinstall.sh — local dev convenience: rebuild, swap binary, edit config.
#
# Steps:
#   1. ghost-complete uninstall   (remove shell integration block from ~/.zshrc)
#   2. cargo uninstall ghost-complete   (drop the cargo-managed binary)
#   3. cargo install --path crates/ghost-complete --locked --force
#      (--force handles the case where step 2 was a no-op because the
#      installed binary was placed by some other path — e.g. a manual
#      `cp` into $CARGO_HOME/bin — and cargo therefore has no record
#      of it to uninstall.)
#   4. ghost-complete install   (re-write shell integration)
#   5. ghost-complete config edit   (open the TUI editor)
#
# Steps 1-2 are best-effort — the script does not fail if there is nothing
# to uninstall (fresh checkout, first run). The cargo install step IS
# fatal: if the rebuild fails we don't want to leave the user with no
# binary on PATH.

set -u
set -o pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

step() {
    printf '\n\033[1;36m==>\033[0m \033[1m%s\033[0m\n' "$*"
}

warn() {
    printf '\033[1;33m[warn]\033[0m %s\n' "$*" >&2
}

step "Removing shell integration"
if command -v ghost-complete >/dev/null 2>&1; then
    ghost-complete uninstall || warn "ghost-complete uninstall reported a non-zero exit; continuing"
else
    warn "ghost-complete not on PATH; skipping shell-integration uninstall"
fi

step "Uninstalling cargo binary"
cargo uninstall ghost-complete 2>/dev/null || warn "cargo had no ghost-complete to uninstall; continuing"

step "Building and installing from $(pwd)"
if ! cargo install --path crates/ghost-complete --locked --force; then
    printf '\n\033[1;31m[fail]\033[0m cargo install failed; aborting before shell-integration step.\n' >&2
    exit 1
fi

step "Reinstalling shell integration"
ghost-complete install

step "Opening config editor"
ghost-complete config edit

cat <<'NOTE'

[3m===========================================================================[0m
[1;33mHeads up:[0m the running ghost-complete PTY proxy in your current shell is
the OLD binary — proxies are spawned at shell startup and do not hot-reload
their own code (only config.toml). To exercise the new binary:

  • Open a new terminal, OR
  • In this shell run:  pkill ghost-complete && exec zsh

[3m===========================================================================[0m
NOTE
