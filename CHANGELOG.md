# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.2] - 2026-03-15

### Added

- **`max_history_results` config field** — controls how many history entries appear in the popup (default: 5). Set to `0` to disable history entirely, which also skips loading `$HISTFILE` at startup. Replaces the binary `providers.history` toggle with a single numeric knob.

### Changed

- **`providers.history` removed from config** — replaced by `max_history_results`. Existing configs with `providers.history` continue to parse without error (the field is silently ignored).
- **History display cap** — history entries in the popup are now capped to `max_history_results` (default 5) after fuzzy scoring, regardless of how many slots remain in `max_results`. Previously, history could fill all remaining popup slots.

## [0.2.1] - 2026-03-15

### Changed

- **History entries insert full command on accept** — selecting a history entry from the popup now replaces the entire command buffer with the full historical command (e.g., `tmux source ~/.config/tmux/tmux.conf`), not just the first word (`tmux`).
- **Buffer-wide history matching** — history entries are fuzzy-matched against the full typed buffer at any word position, not just at command position. Typing `git push` surfaces `git push origin main` from history.
- **History suppressed in compound commands** — history entries no longer appear after pipe (`|`), chain (`&&`, `||`), or semicolon (`;`) operators. Full commands don't make sense as pipe/chain segments.
- **History result cap** — history results are capped to remaining `max_results` slots after main suggestions, preventing unbounded combined result sets.

### Added

- **`is_first_segment` field on `CommandContext`** — tracks whether the cursor is in the first command segment (before any `|`, `&&`, `||`, `;`). Used to gate history suggestions.

## [0.2.0] - 2026-03-14

### Added

- **706 Fig-compatible completion specs (34 → 706)** — converted from @withfig/autocomplete using offline Node.js converter (`tools/fig-converter/`). All specs embedded into the binary via `include_str!`. ~450 pure static, ~190 with script generators, ~66 with `requires_js` (static portions functional).
- **Script generators with async execution** — specs can define shell commands as generators (e.g., `["brew", "list", "-1"]`). Commands execute asynchronously with configurable timeout (default 5s). Results merge into the popup without resetting user's cursor position.
- **Transform pipeline** — composable output transforms for script generators: `split_lines`, `filter_empty`, `trim`, `skip_first`, `dedup`, `split_on(delim)`, `skip(n)`, `take(n)`, `regex_extract(pattern, groups)`, `json_extract(fields)`, `column_extract(cols)`, `error_guard(pattern)`. Validated at spec load time.
- **Generator result caching** — in-memory TTL cache for script generator results. Configurable per-generator with `cache_by_directory` option for CWD-scoped caching.
- **`ghost-complete status` subcommand** — shows loaded spec count, fully/partially functional breakdown, and lists commands requiring JS generators.
- **`ghost-complete doctor` subcommand** — health checks for shell integration, Ghostty detection, config validation (including all theme fields), and spec loading.
- **`ghost-complete config` subcommand** — dumps resolved configuration as TOML for debugging.
- **Scroll-to-make-room popup rendering** — popup always renders below the cursor. When near the bottom of the viewport, the terminal is scrolled to create space instead of rendering above. Scroll deficit persists across dismiss/re-trigger cycles. Popup dismissed on terminal resize.
- **Theme expansion** — three new theme fields: `match_highlight` (style for fuzzy-matched characters), `item_text` (style for non-selected rows), `scrollbar` (scrollbar track/thumb style). All configurable via `[theme]` in config.
- **Theme presets** — four built-in presets selectable via `preset = "dark"` in config: `dark` (default), `light`, `catppuccin`, `material-darker`.
- **Hex truecolor support** — `fg:#RRGGBB` and `bg:#RRGGBB` style tokens in theme configuration.
- **Fuzzy match character highlighting** — matched characters in popup items are visually highlighted using the `match_highlight` theme style.
- **Scrollbar indicator** — scrollable popup lists display a scrollbar when content exceeds the visible area.
- **ghost-complete self-completion spec** — autocomplete for ghost-complete's own subcommands and options.
- **claude and codex completion specs** — added specs for AI CLI tools.
- **Criterion benchmarks** — benchmark suites for `gc-suggest` (fuzzy ranking, spec loading, spec resolution, transform pipeline, engine) and `gc-parser` (VT parse throughput). Manually-triggered CI workflow for benchmark runs.

### Changed

- **`generator_timeout_ms` config option** — global timeout for shell command generators (default 5000ms).
- **`script_template` support** — generators can use `{current_token}` substitution in command arguments.
- **Binary size reduced from 104MB to 25MB** — dropped 11 oversized/niche specs: `aws` (53MB), `gcloud` (22MB), `hub` (deprecated), `fin`, `northflank`, `cl`, `commercelayer`, `sfdx`, `twilio`, `doppler`, `mongocli`.
- **`item_text` default changed from `dim` to empty** — non-selected rows now render with no extra styling by default.

### Fixed

- **Item text color bleed** — style is now reset before rendering descriptions, preventing `item_text` color from bleeding into description text.
- **Scroll deficit lost on dismiss** — scroll deficit now persists across dismiss/re-trigger cycles so the popup doesn't jump.
- **Doctor validates all theme fields** — `doctor` now checks all 5 theme fields (`selected`, `description`, `match_highlight`, `item_text`, `scrollbar`), not just the original 2.

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

[0.2.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.2
[0.2.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.1
[0.2.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.0
[0.1.4]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.4
[0.1.3]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.3
[0.1.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.2
[0.1.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.1
[0.1.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.0
