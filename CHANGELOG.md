# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Corrected

- **Substring/slice misconversion.** The spec converter previously emitted
  `column_extract` for `.substring(0, N)` and `.slice(0, N)` patterns, which are
  byte-offset operations, not whitespace-delimited columns. Affected generators
  now correctly report as requires-JS until a proper fix lands in Phase 2/3A.
  Affected specs: chezmoi, pass, pre-commit.
- **JSON.parse silent fallback.** When `JSON.parse` appeared without a resolvable
  field access, the converter silently emitted `{type: "json_extract", name: "name"}`,
  producing wrong completions. These generators now report as requires-JS.
  Affected specs: docker, podman.

Generators affected by either correction are tagged in the embedded specs with
`_corrected_in: "v0.10.0"`. `ghost-complete doctor` surfaces the count and names
them under the new corrected-generator warning check so users know which specs
silently changed behaviour.

`git.json` and `cd.json` have related deferred work tracked in
`docs/phase-minus-1-followups.md`.

## [0.9.1] - 2026-04-20

### Fixed

- **`git checkout <TAB>`** — native git ref generators now consistently surface branches/tags/remotes above history and filesystem residuals. When refs are still pending, the sync pass suppresses commands/options/history (but preserves filesystem candidates so `git checkout <path>` still works). The dynamic merge's empty-query branch sorts by `SuggestionKind` priority, so refs land at the top instead of being appended after sync residuals. (#73)

### Changed

- **README** — embed demo videos from `assets/` as mp4 (h.264) with clickable poster images; point at v0.9.0 release assets for hosted playback.

## [0.9.0] - 2026-04-19

### Added

- **VSCode terminal support** — Ghost Complete now runs as a first-class PTY proxy inside VSCode's integrated terminal, plus **VSCodium, Cursor, Windsurf, Positron, and Trae** (all detected via `VSCODE_IPC_HOOK_CLI`). Capability profile: `Synchronized` (DECSET 2026 via xterm.js) + native OSC 133. Coexists with VSCode's own shell integration (OSC 633) — the proxy forwards editor sequences untouched so command decorations, sticky scroll, and "run recent command" continue to work. Previously `shell/init.zsh`'s allowlist did not match VSCode, so users got a plain interactive shell with no proxy; it is now a first-class supported target.
- **Zed terminal support** — first-class support for Zed's integrated terminal, detected via `ZED_TERM=true`. Same capability profile as Ghostty/Kitty (Synchronized + native OSC 133).
- **`supported_terminals()`** grows from 7 to 9. `Terminal::Zed` and `Terminal::VSCode` enum variants added to `gc-terminal`; `for_zed()` and `for_vscode()` test constructors available behind `test-utils`.

### Changed

- **`shell/ghost-complete.{zsh,bash,fish}`** — introduced a `_gc_native_osc133` helper that short-circuits OSC 7771 emission when the host terminal already injects native OSC 133 (Ghostty, Zed) or its own shell integration that emits OSC 133 alongside proprietary markers (VSCode, signalled by `VSCODE_INJECTION=1`). Eliminates redundant/conflicting prompt markers in editor-hosted terminals.
- **`shell/init.zsh`** — non-tmux branch now resets an inherited `GHOST_COMPLETE_ACTIVE` when the parent process is not `ghost-complete`. This fixes the `code .` flow: a user launching VSCode from a ghost-complete-managed shell now gets the proxy in VSCode's integrated terminal instead of short-circuiting on the leaked env var. Tmux branch: `$ZED_TERM` and `$VSCODE_IPC_HOOK_CLI` added to the supported-terminals allowlist.

## [0.8.2] - 2026-04-18

### Added

- Embedded specs auto-materialize to `~/.cache/ghost-complete/embedded-specs/` on first run when no user-installed specs are found (enables zero-config `cargo install ghost-complete` usage).
- Logging section in README and CONFIGURATION docs explaining `--log-level`, `--log-file`, `RUST_LOG`, and default log path.
- `publish = false` on the `ghost-complete` binary crate to prevent accidental publish to crates.io.
- `--version` output now includes the git short SHA and build timestamp.
- SBOM / build provenance attestation on release artifacts.
- `deny.toml` + `cargo-deny` license/bans gate.
- Linux CI tripwire to catch accidental Darwin-only regressions.
- End-to-end smoke test covering the keystroke → popup → dismiss lifecycle.
- `cargo audit` workflow — runs `rustsec/audit-check@v2` on pushes to master, on PRs that touch `Cargo.toml`/`Cargo.lock`, and on a weekly cron to catch advisories filed mid-week.
- Release CI gated on `cargo test` / `cargo clippy -D warnings` / `cargo fmt --check` — a red-on-master tag push can no longer ship a release.
- Optional `lefthook` pre-commit hooks (fmt + clippy) mirroring CI. Opt-in via `brew install lefthook && lefthook install`.

### Changed

- **Spec resolution/loading perf regression fix** — removed the eager `OptionsIndex` HashMap (rebuilt per-subcommand descent for zero benefit vs. linear scan over <200 options) and replaced the `chars().any(is_control)` precheck with a flat byte-scan `has_control_char` (catches C0 directly and C1 via a two-byte UTF-8 match). Restores pre-audit numbers: `spec_resolution/shallow` ~3.17µs (was 4.46µs), `spec_resolution/deep` ~1.60µs (was 2.61µs), `spec_loading/load_717` ~70.7ms (was 94.8ms).
- **`validate-specs` performs deep validation** — regex patterns, transform pipelines, and generator types are now checked (previously only top-level JSON parse). New `--strict` flag promotes warnings to a non-zero exit.
- **Spec load-path hardening** — `Arc<GeneratorSpec>` eliminates per-keystroke deep clones; `sanitize_spec_strings` scrubs control chars from every user-visible string field; unknown generator types log a `warn!` on load; `validate_spec_generators` is iterative (no stack recursion on deep subcommand chains).
- **Suggest-engine perf** — in-place transform pipeline (no vec clone per stage); `Vec<Arc<str>>` for `$PATH`; `HashSet<&str>` in `try_merge_dynamic`; `Vec<char>` for trigger chars.
- **Frecency hardening** — merge-on-save (two-terminal union, max of decayed scores), schema-version envelope, 1e18 score clamp, `$XDG_STATE_HOME` respected with one-shot migration.
- **History loading** — tail-read for files >2MiB; (mtime,size) fingerprints replace mtime-only; zsh multi-line entry merge; strict-UTF-8 line validation (invalid lines skipped with `debug!` log instead of silently corrupted with U+FFFD).
- **Alias cache** now tracks every file the fast path reads (`.zsh_aliases`/`.aliases`/`.bash_aliases`), `.*.local` overrides, and a depth-bounded recursive mtime walk over `~/.oh-my-zsh/custom` to catch in-place drop-in edits (directory mtime alone misses them). Cry-wolf log dropped to `debug`.
- **CPR queue** — soft/hard caps with drop-oldest in `gc-parser`.
- **Popup trigger fingerprint guard** — fixes re-trigger-on-unchanged dismissing a visible popup with no re-render.
- **Terminal detection** — `GHOSTTY_RESOURCES_DIR` now requires an existing directory (parity with socket-based detection); a stale env var no longer misidentifies the terminal.
- **Config TUI editor** — array field round-trip, `Event::Resize` redraw, unsaved-change quit confirmation, mtime compare-and-swap on save, panic hook that restores the terminal, bounded backup suffix loop, `NotFound`-tolerant pre-backup path.
- **CLI** — `status --strict` flag + non-zero exit; `config` dump preserves comments via `toml_edit::DocumentMut`; `doctor` exercises the real `resolve_spec_dirs` + `SpecStore::load_from_dirs` chain and FAILs with an actionable message when zero specs load.
- **Build provenance** — `build.rs` emits git short SHA + UTC build timestamp into `--version` (tolerant of missing `.git`); `CARGO_HOME` path remap in `.cargo/config.toml`; release-artifact subject-path globs narrowed.
- Bash and fish shell integration scripts are idempotent when sourced multiple times (mirrors existing zsh behavior).
- Bash DEBUG trap chains with any pre-existing user trap instead of overwriting silently.
- Generator-drop message promoted from `debug` to `warn` so the proxy path surfaces broken pipelines at the same level as `validate-specs`.
- Workspace `tokio` features narrowed to the minimum required set (`rt-multi-thread`, `macros`, `io-util`, `fs`, `process`, `signal`, `time`, `sync`). Drops unused `net` feature and its `socket2` + `parking_lot` transitive deps.
- Clippy cleanup — collapse redundant match guards in overlay types and TUI editor; swap `map_or(true, …)` for `is_none_or(…)`; sync documented MSRV to `1.86` across AGENTS.md / CONTRIBUTING.md / Cargo.toml.

### Fixed

- **`SIGTERM` / `SIGHUP` no longer skip frecency flush** — PTY proxy now registers `SIGTERM` and `SIGHUP` listeners alongside `SIGWINCH` and breaks out to the existing cleanup block. Previously external `kill` or terminal hangup aborted tokio tasks before `flush_frecency()` / `config_watcher_handle.shutdown()` could run, losing accumulated frecency state on every non-EOF exit.
- **`RUST_LOG` now respected; empty `$SHELL` treated as missing** — `init_tracing` prefers `RUST_LOG` and falls back to `--log-level` only when the env var is unset or invalid. `resolve_default_shell` falls back to `/bin/zsh` when `$SHELL` is empty (previously propagated a cryptic `ENOENT` from the PTY spawn).
- Documentation drift: `docs/IMPLEMENTATION_PLAN.md` references now point to `docs/ARCHITECTURE.md`; MSRV documented as `1.75` corrected to `1.86`; crate count documented as `7` corrected to `8`; `theme.border` field added to the config table; spec counts synced to `709` specs / `184` requires_js.
- `rust_out` stray binary removed from repo root and added to `.gitignore`.
- Prior audit docs (`AUDIT_FINDINGS.md`, `AUDIT_RESOLUTION_PLAN.md`) marked archived.

### Security

- **`$HISTFILE` validated** — must canonicalize under canonicalized `$HOME` (catches symlinks escaping `$HOME`) and the basename must match a known history-filename pattern. On rejection, log a `warn!` and fall back to `~/.zsh_history` (which itself re-validates). Blocks arbitrary file reads via env var (e.g. `HISTFILE=/etc/passwd`).
- **Script generator argv rejects NUL bytes** — the only char that truncates argv on Unix. `substitute_template` substitutes empty for NUL-containing tokens at template time; `run_script` rejects any argv element containing NUL as defense-in-depth. Removed misleading shell-metacharacter warning — the exec path uses an argv array (not `sh -c`), so `|`, `;`, `&`, backtick, `$` are inert literals.
- **Spec text ANSI injection** — external spec `name` / `description` / subcommand / option fields are C0/CSI/OSC-stripped at load time (`sanitize_spec_strings`). Blocks terminal-escape injection via malicious user-installed specs; mirrors the render-side sanitizer for defense in depth.
- **Spec JSON depth cap** — external spec JSON rejected above 32 levels (flat byte-scan preflight, `check_json_depth`) to prevent stack overflow from nested-subcommand DoS (serde_json's default 128 was far above our deepest real-world spec at 15). `validate_spec_generators` is also iterative so a future cap relaxation cannot re-introduce the overflow.

## [0.8.1] - 2026-04-17

### Fixed

- **Ctrl+L 5s hang under z4h** — replaced the `cpr_pending: u8` counter with a FIFO request queue tagged by origin (`Ours` / `Shell`). Terminals respond to `CSI 6n` in request order, so popping the head dispatches each response to the correct owner without timing heuristics. Eliminates the class of races where overlapping CPRs collapsed into one flag, our-pending-then-shell starved the shell, and the 500ms-vs-5s expiry mismatch stalled redraws.
- **RPROMPT misalignment under p10k + z4h** — `_gc_report_buffer` is now chained into `zle-line-pre-redraw` directly instead of via `add-zle-hook-widget`. The hook-widget dispatcher renames `$WIDGET` to `azhw:zle-line-pre-redraw`, which broke z4h's `_zsh_highlight()` guard and caused syntax highlighting to run during prompt rendering — inflating the width measurement p10k uses for RPROMPT alignment. The chaining installer is idempotent and preserves any existing `zle-line-pre-redraw` widget identity.
- **Write-failure recovery for CPR requests** — `rollback_cpr` removes a pending `Ours` entry by token if the `CSI 6n` write fails, so a dropped request doesn't permanently steal a response slot. Stale `Ours` entries older than 30s are pruned once per proxy iteration as a leak guard against terminals that silently drop requests.

### Changed

- **Parser CPR API** — `cpr_pending` / `increment_cpr_pending` / `claim_cpr_response` removed in favor of `enqueue_cpr` / `claim_next_cpr` / `rollback_cpr` / `prune_stale_cpr` / `cpr_queue_len` with a `VecDeque<CprEntry>` backing store. `CprOwner` and `CprToken` are re-exported from `gc-parser`. `CSI 6n` auto-enqueues a `Shell` entry in the `vte::Perform` impl.
- **Proxy CPR dispatch** — extracted into `dispatch_cpr_response` helper; Task A pops the queue head and matches on owner, Task B enqueues `Ours` with a token and rolls back on write failure.
- **Shell integration test coverage** — new `tests/shell/test_zle_chaining.zsh` asserts the ZLE wrapper preserves `$WIDGET` and is idempotent across both install branches (chain and direct-install).

## [0.8.0] - 2026-04-12

### Added

- **Config TUI editor** — `ghost-complete config edit` opens an interactive terminal UI for editing configuration.

## [0.7.1] - 2026-04-12

### Fixed

- **Security hardening** — ANSI escape sequences in suggestion text are sanitized (prevents terminal injection via spec output). Shell-injecting paths in `.zshrc` init blocks are properly quoted. OSC 7 CWD reports rejected on path traversal; OSC 7770 buffer reports rejected on non-UTF-8 data. CPR response validation added. Terminal detection hardened — env var sanitization, stale socket detection, `TERM_PROGRAM` sanitization.
- **Crash resistance** — mutex poison recovery across all 12 proxy lock sites (a panic in one task no longer cascades). Narrow-terminal layouts no longer panic; defense-in-depth guard in `compute_layout`. Saturating arithmetic for u16 overflow, zero-dimension clamping, bounds-safe `spec_dirs` indexing, `byte_to_char_offset` clamping, double-panic recovery.
- **Unicode correctness** — wide characters (CJK, emoji) use `unicode-width` terminal column width with a CJK early-wrap branch. Cursor restore clamped after terminal resize. CPR count desync race eliminated.
- **Tokenizer parity** — `#` comments, FD redirects (`2>&1`), heredocs (`<<EOF`), here-strings (`<<<"x"`), and nested command substitution (`$(echo $(date))`) are now parsed correctly.
- **Stuck loading indicator** — dynamic popup spinner no longer gets stuck on empty/stale/disconnected generator results.
- **Orphaned generator tasks** — async generator tasks are aborted via `JoinHandle` on dismiss, preventing leaked tasks from older triggers.
- **Config robustness** — TOCTOU-safe load, atomic `create_new` for default config writes, hot-reload warnings on restart-required fields, non-UTF-8 config paths rejected with warning, unknown-key warnings via two-pass TOML load.
- **Overlay regressions** — `GUTTER_COLS` constant prevents nerd-font gutter math drift, loading+border deficit formula corrected, `scroll_offset` resets on deselect.
- **Install/doctor polish** — backup overwrite guard, unreadable entry counting, `doctor` is `multi_terminal`-aware, embedded spec counts, root-user guard, uninstall cleanup note.

### Changed

- **Performance** — script-generator stdout bounded at 1 MiB with concurrent stderr drain (prevents runaway memory). Suggestion cache uses LRU eviction via `CACHE_SWEEP_THRESHOLD`. Alias loading moved to async (`AliasStore` + `RwLock`). Frecency record/flush writes moved out of mutex hold. Full async git migration via `tokio::process` (no more blocking threads for git context). Regex patterns precompiled at spec load time.
- **Refactor** — `format_item` extracted into 6 helpers, `suggest_sync` branch logic into 8 helpers. `GUTTER_COLS` / `DESC_GAP_COLS` / `TRAILING_PAD_COLS` named constants replace magic numbers. `resolve_spec_dirs` deduplicated into dedicated module. `ThemeConfig::validate()` provides shape-only pre-load checks. Theme overrides now use `Option<String>` with a `ResolvedTheme` struct.
- **README** — centered header, tightened status language, renamed "What is this?" to "Overview", added star-history.com timeline.

## [0.7.0] - 2026-04-11

### Added

- **`auto_trigger` config flag** — disables all automatic triggers (debounce, auto_chars, CWD change) when set to `false`. Only manual keybinding (Ctrl+/) works. Hot-reloadable — toggling false while the popup is visible dismisses it and clears stale state.

### Fixed

- **CPR response forwarding for Atuin and other PTY programs** — ghost-complete was consuming all Cursor Position Report responses, starving programs like Atuin/crossterm that send their own CSI 6n requests. Now tracks pending CPR count and only consumes responses to its own requests, forwarding the rest through the PTY.

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

[0.9.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.9.1
[0.9.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.9.0
[0.8.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.8.2
[0.8.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.8.1
[0.8.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.8.0
[0.7.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.7.1
[0.7.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.7.0
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
