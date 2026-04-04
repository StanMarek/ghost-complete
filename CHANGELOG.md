# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.1] - 2026-04-04

### Fixed

- **Prevent recursive launch and enable per-pane proxy in tmux** — fixes recursive ghost-complete spawning and enables independent proxy instances per tmux pane.

## [0.6.0] - 2026-04-04

### Added

- **Frecency recording wired in production** — frecency scoring (added in v0.2.3) now records accepted completions. Every Tab/Enter acceptance calls `record_frecency()`, so the frecency database is no longer always empty.
- **Exponential decay algorithm** — replaced linear decay (`freq * 1/(1+t/168h)`) with exponential decay using single-number compression (`stored_score / 2^(t/72h)`). Full usage history compressed into one `f64` per entry. Half-life shortened from 1 week to 3 days.
- **Context-aware frecency keys** — argument completions keyed as `command\0kind\0text` so `--help` under `git` doesn't pollute `docker`. History items always keyed without command scope for consistency.
- **Frecency boosts all suggestion types** — files, flags, branches, subcommands all benefit from frecency boosting, not just history entries. Re-sorts after boosting while preserving history-comes-last ordering.

### Changed

- **Atomic frecency persistence** — writes via tmp+rename, batch saves every 3 accepts, flush on proxy shutdown. Prunes to 1000 entries on save.
- **Schema migration** — old `{frequency, last_used_secs}` format auto-migrated to `{stored_score, reference_secs}` on load.
- **Mutex poison recovery** — `FrecencyDb` lock uses poison recovery so a best-effort subsystem never crashes the proxy.

## [0.5.0] - 2026-04-03

### Added

- **Ctrl+A through Ctrl+Z keybindings** — full alphabet of ctrl keybindings now supported for custom actions.
- **CWD tracking via OSC 7** — filesystem completions now use the shell's actual working directory for accurate path resolution.
- **Rounded border on completion popup** — popup uses rounded Unicode box-drawing characters for a cleaner look.

### Fixed

- **Hardened keybindings, OSC 7 encoding, and border rendering** — fixes for edge cases in keybinding dispatch, percent-encoding in OSC 7 CWD URIs, and border character rendering.

## [0.4.1] - 2026-04-01

### Changed

- **Batch update 5 dependencies** — routine dependency bumps.

## [0.4.0] - 2026-03-31

### Added

- **Kitty, WezTerm, Alacritty, Rio terminal support** — Ghost Complete now supports 7 terminals on macOS. Kitty, WezTerm, and Rio have full parity with Ghostty (DECSET 2026 + OSC 133). Alacritty uses DECSET 2026 with shell integration prompt detection (no native OSC 133).
- **tmux detection for new terminals** — Kitty (`KITTY_WINDOW_ID`), WezTerm (`WEZTERM_UNIX_SOCKET`), and Alacritty (`ALACRITTY_SOCKET`) are now detected inside tmux sessions.

### Removed

- **`min_width` and `max_width` popup config fields** — popup width is now auto-sized. Existing configs with these fields continue to parse without error (silently ignored).
- **`generator_timeout_ms` suggest config field** — generator timeout is now hardcoded. Existing configs with this field continue to parse without error (silently ignored).
- **`max_history_entries` suggest config field** — replaced by `max_history_results` in v0.2.2. Existing configs with this field continue to parse without error (silently ignored).

### Changed

- **Experimental gate removed for known terminals** — all 7 supported terminals work without `[experimental] multi_terminal = true`. The flag now only applies to unknown/unlisted terminals.
- **Init block rewritten** — `.zshrc` init block detects Kitty via `KITTY_WINDOW_ID` before the `TERM_PROGRAM` case (Kitty reports `TERM_PROGRAM=xterm-kitty`). Supported terminals auto-exec without a config gate.
- **`known_term_programs()` renamed to `supported_terminals()`** — returns display names for all 7 terminals instead of `TERM_PROGRAM` values.

## [0.3.0] - 2026-03-28

### Added

- **Multi-terminal support (experimental)** — Ghost Complete now runs on **iTerm2** and **Terminal.app** in addition to Ghostty. Disabled by default; enable with `multi_terminal = true` under `[experimental]` in config.toml. Terminal detection is automatic via `TERM_PROGRAM` allowlist.
- **New `gc-terminal` crate** — encapsulates terminal detection, capability profiling, and render strategy selection. `TerminalProfile` struct with `RenderStrategy` and `PromptDetection` enums provides type-safe terminal abstraction.
- **OSC 7771 prompt boundary protocol** — terminal-agnostic prompt detection emitted by shell integration scripts alongside OSC 133. Works on all terminals regardless of native semantic prompt support.
- **tmux-in-iTerm2 support** — proxy auto-starts in tmux sessions launched from iTerm2 via `ITERM_SESSION_ID` detection.

### Changed

- **Rendering pipeline** — popup rendering conditionally uses DECSET 2026 synchronized output on Ghostty, falls back to pre-render buffer strategy (single `write()` atomicity) on iTerm2 and Terminal.app.
- **Init block** — `.zshrc` init block now uses a `case` statement: Ghostty always auto-execs, iTerm2/Terminal.app auto-exec only when `multi_terminal = true` is set in config (checked via grep at shell startup).
- **`doctor` command** — `check_ghostty()` replaced with `check_terminal()` that reports detected terminal name, render strategy, and prompt detection method. Lists all supported terminals on failure.
- **Shell integration scripts** — zsh, bash, and fish scripts now emit both OSC 133 and OSC 7771 markers for cross-terminal compatibility.

## [0.2.5] - 2026-03-25

### Added

- **`ghost-complete install --dry-run`** — previews what would be installed without writing any files. Shows the exact shell blocks needed for manual configuration.

### Changed

- **Graceful fallback for read-only .zshrc** — when `.zshrc` is not writable (e.g. nix-darwin/home-manager), install now prints colored manual instructions with the exact shell blocks instead of failing with an error. Only `PermissionDenied` triggers the fallback; other write errors propagate normally.
- **Install deploys zsh integration only** — bash and fish shell scripts are no longer deployed during install (not actively supported). Uninstall still cleans up legacy bash/fish scripts from prior installs.
- **Updated CLI help text** — `--help` output reflects zsh-only shell support.

## [0.2.4] - 2026-03-22

### Fixed

- **Popup suppressed during shell history navigation** — up/down arrow keys for history recall no longer trigger the debounce auto-suggest. A `debounce_suppressed` flag gates the debounce path, set on arrow up/down when the popup is hidden and cleared on printable input or manual trigger.
- **Spawned shell inherits parent working directory** — `CommandBuilder` was not inheriting the parent process's CWD, causing the shell to start in `$HOME`. This broke terminal multiplexers (e.g. cmux) that rely on restoring the working directory when reopening sessions. The current directory is now explicitly passed to `CommandBuilder`.

## [0.2.3] - 2026-03-16

### Added

- **Environment variable completion** — typing `$` in argument position suggests environment variables (`$HOME`, `$PATH`, etc.). Pre-filtered by typed prefix.
- **SSH host completion** — `ssh` arguments suggest hosts parsed from `~/.ssh/config`. Mtime-cached, skips wildcards, handles multiple hosts per line.
- **Shell alias resolution** — aliases like `alias g=git` are resolved before spec lookup, so `g push` uses the git spec. Reads dotfiles first (`.zsh_aliases`, `.aliases`, `.bash_aliases`), falls back to non-interactive subprocess with 2-second timeout.
- **Frecency scoring infrastructure** — commands scored by `frequency × recency` (half-life ~1 week). JSON persistence at `~/.config/ghost-complete/frecency.json` with batched saves and pruning to 1000 entries. Recording hook not yet wired — scoring is read-only in this release.
- **Config hot-reload** — watches `config.toml` via `notify` crate. Debounced (200ms), multi-stage validation (parse → theme → styles → keybindings). Invalid edits logged and ignored.
- **Loading indicator** — dimmed `...` footer row in popup when async script generators are pending.
- **Nerd Font icons** in popup gutter — terminal, chevron, flag, file, folder, branch, tag, link, history icons replace single-letter indicators.
- **`display_text()` helper** in `gc-overlay/src/util.rs` — shared basename extraction for consistent width calculation and rendering.
- **Test builders** — `make_visible_handler()` / `make_selected_handler()` in handler tests.

### Changed

- **Trailing space after accept** — accepting a non-directory suggestion appends a space so the user can immediately type the next argument. Skipped for `=`-terminated flags, history entries, and directories.
- **Single spec resolution** per trigger — previously resolved the spec tree 3 times (suggest_sync, has_script_generators, suggest_dynamic). Now `SyncResult` carries pre-resolved generators.
- **Dynamic merge re-ranking** — when async generators return, merged results are re-ranked against the current query.
- **History mtime refresh** — re-reads `~/.zsh_history` when file mtime changes instead of loading once at startup.
- **Unicode-width for popup sizing** — uses `unicode-width` crate for correct CJK/emoji terminal column width (2 columns per fullwidth character).
- **Light theme preset** — distinct colors for light terminal backgrounds (`fg:#1e1e2e bg:#dce0e8` selection, `fg:#d20f39` match highlight).
- **Basename in popup width** — width calculated from displayed basename, not full path.
- **Graceful mutex handling** — all long-lived async task locks use `match` with `tracing` logging on poison. `.unwrap()` retained in `spawn_blocking` I/O tasks where panic = correct termination.
- **Frecency error logging** — corrupt JSON logged at `warn`, unreadable file at `debug`, directory creation failure at `warn`.

### Fixed

- **Loading indicator stale on empty results** — Task E is now always notified when generators finish, even on empty or error results.
- **Description padding with non-ASCII text** — description column padding uses `unicode-width` character width, not byte length.

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

[0.6.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.6.1
[0.6.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.6.0
[0.5.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.5.0
[0.4.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.4.1
[0.4.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.4.0
[0.3.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.3.0
[0.2.5]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.5
[0.2.4]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.4
[0.2.3]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.3
[0.2.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.2
[0.2.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.1
[0.2.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.0
[0.1.4]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.4
[0.1.3]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.3
[0.1.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.2
[0.1.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.1
[0.1.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.0
