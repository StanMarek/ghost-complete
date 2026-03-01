# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-03-01

### Added

- **PTY proxy engine** — transparent proxy between terminal and shell using `portable-pty` and `tokio`
- **VT parser** — escape sequence tracking via `vte` crate for cursor position, prompt boundaries (OSC 133), and CWD (OSC 7)
- **Command buffer reconstruction** — detects current command, argument position, pipes, and redirects
- **Suggestion engine** with providers:
  - Filesystem completions
  - `$PATH` command completions
  - Shell history completions
  - Git context completions (branches, remotes, tags, files)
  - Fig-compatible JSON spec completions
- **Fuzzy ranking** via `nucleo` (<1ms on 10k candidates)
- **ANSI popup rendering** with synchronized output (DECSET 2026), cursor save/restore, above/below positioning
- **18 completion specs**: brew, cargo, cd, curl, docker, gh, git, grep, jq, kubectl, make, npm, pip, pip3, python, python3, ssh, tar
- **Debounce-based auto-trigger** — configurable delay (default 150ms) after typing pauses
- **Manual trigger** via Ctrl+Space (works in zsh, bash, and fish)
- **Configurable keybindings** — accept, dismiss, navigate, trigger actions with fail-fast validation
- **Theme customization** — SGR-based style strings for selected item and description
- **TOML configuration** at `~/.config/ghost-complete/config.toml`
- **Install/uninstall CLI** — idempotent `.zshrc` management, spec deployment, shell script installation
- **Shell integration** for zsh (full), bash (Ctrl+Space), and fish (Ctrl+Space)
- **`validate-specs` subcommand** with colored output and item counts

[0.1.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.0
