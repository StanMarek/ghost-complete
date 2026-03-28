# Ghost Complete

**Terminal-native autocomplete engine using PTY proxying for macOS terminals.**

[![CI](https://github.com/StanMarek/ghost-complete/actions/workflows/ci.yml/badge.svg)](https://github.com/StanMarek/ghost-complete/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/StanMarek/ghost-complete)](https://github.com/StanMarek/ghost-complete/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

https://github.com/user-attachments/assets/5b6a4384-7cf8-4088-9630-3ddf0ff0e93c



## What is this?

Ghost Complete sits inside your terminal's data stream as a PTY proxy, intercepting I/O between your terminal emulator and your shell. When you type a command, it renders autocomplete suggestions as native ANSI popups — no macOS Accessibility API, no IME hacks, no Electron overlay. Just your terminal, your shell, and fast completions.

Inspired by [Fig](https://fig.io) (RIP). Built from scratch in Rust.

## Status

This is a personal project I built for my own workflow. I'm happy to share it and welcome contributions, but set your expectations accordingly:

- **Ghostty + zsh is the primary tested path.** iTerm2 and Terminal.app have experimental support as of v0.3.0 (opt-in via config flag).
- **Bash and fish support is experimental.** Manual trigger only (Ctrl+/), no auto-trigger on typing, and not actively tested.
- **No stability guarantees.** Config format, spec format, and behavior may change between releases.
- **macOS only.** No Linux or Windows support planned at this time.

If you hit a bug, [open an issue](https://github.com/StanMarek/ghost-complete/issues). I'll fix what I can.

## Requirements

- **Terminal:** [Ghostty](https://ghostty.org) (default), [iTerm2](https://iterm2.com) or Terminal.app (experimental — see below)
- **OS:** macOS
- **Shell:** zsh (primary), bash and fish (Ctrl+/ trigger only)
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
- Installs 709 completion specs to `~/.config/ghost-complete/specs/`
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
- **Ctrl+/** to manually trigger completions

Run `ghost-complete status` to see loaded specs and generator diagnostics.

### iTerm2 / Terminal.app (experimental)

Multi-terminal support is available but disabled by default. To enable it, add to `~/.config/ghost-complete/config.toml`:

```toml
[experimental]
multi_terminal = true
```

Then restart your shell or run `source ~/.zshrc`. Ghost Complete will auto-detect your terminal and use the appropriate rendering strategy.

**Known limitation:** Terminal.app inside tmux is not detected (Terminal.app sets no env var that leaks through tmux).

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
trigger = "ctrl+/"

[theme]
preset = "dark"  # dark, light, catppuccin, material-darker

[suggest]
max_results = 50
max_history_results = 5
generator_timeout_ms = 5000

[suggest.providers]
commands = true
filesystem = true
specs = true
git = true

[experimental]
multi_terminal = false  # Set to true for iTerm2/Terminal.app
```

Config changes are applied live — no restart needed.

See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for the full reference.

## Completion Specs

Ghost Complete ships with 709 Fig-compatible JSON completion specs covering git, docker, cargo, npm, kubectl, brew, curl, ssh, and 700+ more — converted from the [Fig](https://fig.io) autocomplete ecosystem.

Beyond specs, built-in providers offer:
- **Environment variables** — `echo $HOM` → `$HOME`
- **SSH hosts** — parsed from `~/.ssh/config` with mtime caching
- **Shell alias resolution** — `alias g=git` → `g push` uses the git spec
- **Frecency-ranked history** — frequently/recently used commands score higher

Many specs include dynamic generators that run shell commands for live results (e.g., `brew list`, `docker ps`, `kubectl get`). Generator results are cached with configurable TTL. A loading indicator (`...`) appears while generators run.

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
| Ctrl+/ manual trigger | Yes | Yes | Yes |
| PTY proxy wrapping | Yes | Yes | Yes |
| OSC 133 prompt markers | Yes | Yes | Yes |

## FAQ

**How is this different from zsh/fish built-in autocomplete?**

Built-in completions work great — Ghost Complete doesn't replace them. It adds a visual popup layer on top, like the difference between typing from memory and having an IDE dropdown. Suggestions are fuzzy-ranked from multiple sources (completion specs, filesystem, git branches, command history) and displayed in a single view. Think of it as complementary, not a replacement.

**Why a PTY proxy instead of a zsh plugin?**

The PTY proxy sits between the terminal and the shell, rendering popups via pure ANSI escape sequences. This means no zle widget conflicts, no plugin manager dependencies, no RPROMPT corruption, and no fragile shell internals to hook into. It's more complex under the hood, but the UX is cleaner — one binary, works immediately after install.

**Why custom JSON specs instead of using the shell's built-in completions?**

Specs are declarative and fast — microsecond loads, no shell execution. They use the same format [Fig](https://fig.io) used, so there's a large existing ecosystem to draw from. Ghost Complete ships with 709 specs today, and many include dynamic generators that execute shell commands for live results (e.g., listing running containers, git branches, installed packages). Commands without a spec fall back to filesystem completions. Adding new specs is straightforward — see [docs/COMPLETION_SPEC.md](docs/COMPLETION_SPEC.md), and contributions are welcome.

**Where's the config documentation? I'm having popup alignment issues.**

Full config reference lives at [docs/CONFIGURATION.md](docs/CONFIGURATION.md). Running `ghost-complete install` generates a commented default config at `~/.config/ghost-complete/config.toml` with all available options.

For popup alignment: Ghost Complete uses ANSI cursor positioning within the terminal grid, so popups always track the cursor position directly. This avoids the window-level coordinate issues that plague Accessibility API approaches (the kind of drift reported with tools like Amazon Q / Kiro). If popups are misaligned, it's likely a terminal compatibility issue — please [open an issue](https://github.com/StanMarek/ghost-complete/issues) with your setup details.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE)
