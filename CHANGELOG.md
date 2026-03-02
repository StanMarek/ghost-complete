# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-03-02

### Fixed

- **Multi-byte UTF-8 crash** — typing non-ASCII characters (e.g., `ą`, `ś`) no longer panics and kills the terminal session. Tokenizer rewritten to iterate over characters instead of raw bytes; cursor offset conversion from character to byte boundaries added throughout.
- **History suggestions polluting top results** — history completions now always sort after non-history suggestions, preserving score order within each group
- **`cd` showing files instead of directories** — spec resolution now takes priority over the `looks_like_path` heuristic, so `cd Desktop/` correctly filters to directories only
- **Accidental suggestion insertion on fast typing** — popup no longer auto-selects the first item. Tab and Enter with no selection forward the keystroke to the shell instead of inserting the top suggestion.

### Added

- **`../` parent directory shortcut for `cd`** — shown as the first suggestion when the current word is empty, with support for chaining (`../../`). Hidden at `/` and `$HOME` boundaries.

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

[0.1.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.1
[0.1.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.0
