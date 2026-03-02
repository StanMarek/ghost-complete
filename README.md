# Ghost Complete

**Terminal-native autocomplete engine using PTY proxying, built for Ghostty.**

[![CI](https://github.com/StanMarek/ghost-complete/actions/workflows/ci.yml/badge.svg)](https://github.com/StanMarek/ghost-complete/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/StanMarek/ghost-complete)](https://github.com/StanMarek/ghost-complete/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

<!-- TODO: add demo GIF -->



https://github.com/user-attachments/assets/3679a74a-731d-468a-bdda-bec926bb2349




## What is this?

Ghost Complete sits inside your terminal's data stream as a PTY proxy, intercepting I/O between Ghostty and your shell. When you type a command, it renders autocomplete suggestions as native ANSI popups — no macOS Accessibility API, no IME hacks, no Electron overlay. Just your terminal, your shell, and fast completions.

Inspired by [Fig](https://fig.io) (RIP). Built from scratch in Rust.

## Requirements

- **Terminal:** [Ghostty](https://ghostty.org)
- **OS:** macOS
- **Shell:** zsh (primary), bash and fish (Ctrl+Space trigger only)
- **Rust:** 1.75+ (for building from source)

## Installation

### Homebrew (recommended)

```bash
brew install StanMarek/tap/ghost-complete
ghost-complete install
```

### Shell installer

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/StanMarek/ghost-complete/releases/latest/download/ghost-complete-installer.sh | sh
ghost-complete install
```

### Cargo

```bash
cargo install --git https://github.com/StanMarek/ghost-complete.git
ghost-complete install
```

### From source

```bash
git clone https://github.com/StanMarek/ghost-complete.git
cd ghost-complete
cargo build --release
cp target/release/ghost-complete ~/.cargo/bin/
ghost-complete install
```

### What `ghost-complete install` does

- Adds shell integration to `~/.zshrc` (auto-wraps your shell via PTY proxy)
- Deploys shell scripts for bash/fish to `~/.config/ghost-complete/shell/`
- Installs 18 completion specs to `~/.config/ghost-complete/specs/`
- Creates default config at `~/.config/ghost-complete/config.toml` (never overwrites existing)

### Uninstall

```bash
ghost-complete uninstall
brew uninstall ghost-complete  # if installed via Homebrew
```

## Quick Start

After installation, restart your terminal. Ghost Complete activates automatically in zsh.

- **Type a command** and suggestions appear after a short delay
- **Tab** to accept the selected suggestion
- **Enter** to accept and execute
- **Arrow keys** to navigate the popup
- **Escape** to dismiss
- **Ctrl+Space** to manually trigger completions

## Configuration

Config lives at `~/.config/ghost-complete/config.toml`:

```toml
[trigger]
auto_chars = [' ', '/', '-', '.']
delay_ms = 150

[popup]
max_visible = 10
min_width = 20
max_width = 60

[keybindings]
accept = "tab"
dismiss = "escape"
trigger = "ctrl+space"

[theme]
selected = "reverse"
description = "dim"
```

See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for the full reference.

## Completion Specs

Ghost Complete ships with 18 Fig-compatible JSON completion specs:

`brew` `cargo` `cd` `curl` `docker` `gh` `git` `grep` `jq` `kubectl` `make` `npm` `pip` `pip3` `python` `python3` `ssh` `tar`

Custom specs go in `~/.config/ghost-complete/specs/`. See [docs/COMPLETION_SPEC.md](docs/COMPLETION_SPEC.md) for the format reference.

## Architecture

Rust workspace with 7 crates:

| Crate | Role |
|-------|------|
| `ghost-complete` | Binary entry point, CLI, install/uninstall |
| `gc-pty` | PTY proxy event loop (portable-pty + tokio) |
| `gc-parser` | VT escape sequence parsing (vte), cursor/prompt tracking |
| `gc-buffer` | Command line reconstruction, context detection |
| `gc-suggest` | Suggestion engine with fuzzy ranking (nucleo) |
| `gc-overlay` | ANSI popup rendering with synchronized output |
| `gc-config` | TOML config, keybindings, themes |

See [docs/IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) for the full design.

## Shell Support

| Feature | zsh | bash | fish |
|---------|-----|------|------|
| Auto-trigger on typing | Yes | No | No |
| Ctrl+Space manual trigger | Yes | Yes | Yes |
| PTY proxy wrapping | Yes | Yes | Yes |
| OSC 133 prompt markers | Yes | Yes | Yes |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE)
