# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] - v0.2.0

### Added

- **717 Fig-compatible completion specs (34 → 717)** — converted from @withfig/autocomplete using offline Node.js converter (`tools/fig-converter/`). All specs embedded into the binary via `include_str!`.
- **Script generators with async execution** — specs can define shell commands as generators (e.g., `["brew", "list", "-1"]`). Commands execute asynchronously with configurable timeout (default 5s). Results merge into the popup without resetting user's cursor position.
- **Transform pipeline** — composable output transforms for script generators: `split_lines`, `filter_empty`, `trim`, `skip_first`, `dedup`, `split_on(delim)`, `skip(n)`, `take(n)`, `regex_extract(pattern, groups)`, `json_extract(fields)`, `column_extract(cols)`, `error_guard(pattern)`. Validated at spec load time.
- **Generator result caching** — in-memory TTL cache for script generator results. Configurable per-generator with `cache_by_directory` option for CWD-scoped caching.
- **`ghost-complete status` subcommand** — shows loaded spec count, fully/partially functional breakdown, and lists commands requiring JS generators.
- **Criterion benchmarks** — benchmark suites for `gc-suggest` (fuzzy ranking, spec loading, spec resolution, transform pipeline, engine) and `gc-parser` (VT parse throughput). Manually-triggered CI workflow for benchmark runs.

### Changed

- **`generator_timeout_ms` config option** — global timeout for shell command generators (default 5000ms)
- **`script_template` support** — generators can use `{current_token}` substitution in command arguments

## [0.1.4] - 2026-03-12

### Fixed

- **Popup rendering artifacts from long suggestions** — suggestion text (history URLs, deep paths) was written to the render buffer without truncation, overflowing past the popup's declared width. `clear_popup` only erased `layout.width` columns, leaving ghost characters on screen until a terminal resize. Text is now truncated to fit within the popup boundary.
- **Redundant path prefix in filesystem completions** — directory/file suggestions now display only the last path component (e.g., `2023-rust/` instead of `Desktop/coding/project/2023-rust/`), since the user already typed the prefix.

## [0.1.3] - 2026-03-10

### Added

- **16 new completion specs (18 → 34 total)** — tmux (85 subcommands), rustup (36 subcommands), node (57 options), wget, rsync, find, chmod, kill, killall, zip, unzip, ln, man, mvn, gradle, gradlew
- **tmux-in-Ghostty support** — ghost-complete now activates inside tmux sessions launched from Ghostty. Uses a PPID-based guard instead of `GHOST_COMPLETE_ACTIVE` env var to avoid inheritance through tmux. Adds tmux version logging at proxy startup.

### Fixed

- **Init block firing in non-Ghostty terminals** — the `.zshrc` init block now checks `TERM_PROGRAM == "ghostty"` before exec'ing ghost-complete, so VS Code integrated terminal, iTerm2, Terminal.app, etc. are no longer affected

## [0.1.2] - 2026-03-02

### Changed

- **Default trigger keybinding changed from Ctrl+Space to Ctrl+/** — Ctrl+Space (`0x00`) conflicts with tmux's prefix key, preventing the trigger from working inside tmux sessions. Ctrl+/ (`0x1F`) is distinct and unused by tmux or readline defaults. Users who prefer the old binding can set `trigger = "ctrl+space"` in their config.

### Added

- **`ctrl+/` key name** — now recognized by the keybinding parser alongside existing key names

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
- **Manual trigger** via Ctrl+/ (works in zsh, bash, and fish)
- **Configurable keybindings** — accept, dismiss, navigate, trigger actions with fail-fast validation
- **Theme customization** — SGR-based style strings for selected item and description
- **TOML configuration** at `~/.config/ghost-complete/config.toml`
- **Install/uninstall CLI** — idempotent `.zshrc` management, spec deployment, shell script installation
- **Shell integration** for zsh (full), bash (Ctrl+/), and fish (Ctrl+/)
- **`validate-specs` subcommand** with colored output and item counts

[0.1.4]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.4
[0.1.3]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.3
[0.1.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.2
[0.1.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.1
[0.1.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.0
