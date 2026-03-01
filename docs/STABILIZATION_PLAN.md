# Ghost Complete — Stabilization Plan

> Audit performed 2026-02-28. Covers all 7 crates + shell integration.
> This document tracks what's done, what's broken, and what needs work before this thing is daily-driver ready.

---

## Current State: 100% Stabilization Complete — All Phases Done

### What's Solid

| Area | Crate | Tests | Verdict |
|------|-------|-------|---------|
| PTY proxy & event loop | `gc-pty` | 41 + 10 smoke | Rock solid. CPR cursor sync, configurable keybindings, no deadlocks, proper cleanup. |
| VT escape sequence parsing | `gc-parser` | 39 | Cursor tracking with CPR ground-truth sync, OSC 133/7/7770. |
| Command buffer tokenization | `gc-buffer` | 28 | Pipes, redirects, quotes, escapes — handles real shell syntax. |
| Suggestion engine core | `gc-suggest` | 52 | Multi-provider, template/generator support, nucleo fuzzy <1ms. |
| Popup rendering | `gc-overlay` | 42 | Flicker-free (DECSET 2026), scrollback-safe, smart positioning, themeable. |
| Configuration | `gc-config` | 8 | TOML config with keybindings + theme support, sensible defaults, serde deserialization. |
| Install/uninstall CLI | `ghost-complete` | 14 | Idempotent .zshrc management, spec deployment, multi-shell script deployment, default config.toml generation, backup on install. |
| **Total** | | **234** | **All passing, clippy clean, no TODOs/FIXMEs in codebase.** |

### Completion Providers (5 implemented)

| Provider | Source | Status |
|----------|--------|--------|
| Filesystem | `tokio::fs` / `std::fs` | Working — async-ready but currently sync |
| Git | CLI (`git branch`, `git tag`, `git remote`) | Working |
| History | `~/.zsh_history` (extended format) | Working — deduplication, eager load |
| Commands | `$PATH` scan | Working |
| Specs | Fig-compatible JSON in `specs/` | Working — 18 specs loaded |

### Completion Specs (18 total)

**Original 6:** git, docker, cargo, npm, kubectl, node
**Added 12:** brew, ssh, curl, gh, make, jq, tar, grep, python, python3, pip, pip3

---

## Priority Issues — Ranked by Daily-Use Impact

### P0: Template/Generator Support in Specs

**Status:** DONE (2026-02-28)

Two bugs fixed:
1. `resolve_spec()` didn't check `preceding_flag`'s option args for templates/generators — added preceding_flag lookup before positional args check.
2. When cursor is at option arg position (e.g., `curl -o <TAB>`), all 74 flag candidates were added before filesystem entries, and empty-query truncation at 50 results cut all files. Added `in_option_arg` guard in `engine.rs` to skip subcommands/options when filling option arg.

Verified: `curl -o <TAB>`, `pip install -r <TAB>`, `cd <TAB>` all work correctly.

---

### P0.5: Popup Cursor Drift with P10k

**Status:** DONE (2026-02-28)

Three bugs fixed:
1. `line_feed()` and `advance_col()` in `state.rs` didn't clamp cursor_row to screen_rows — cursor grew unbounded past screen size, causing infinite popup drift.
2. `CSI n S` (scroll up) and `CSI n T` (scroll down) in `performer.rs` incorrectly called `move_up()`/`move_down()` — these sequences scroll content, NOT the cursor. Changed to no-ops. Also added `CSI s`/`CSI u` (ANSI save/restore cursor) handling.
3. **CPR cursor sync** — fundamental fix. VT parser can't perfectly track P10k's complex cursor sequences, so cursor_row drifts. Added Device Status Report (CSI 6n) mechanism: on every OSC 133;A (prompt start), the proxy queries the REAL terminal for actual cursor position. Response is intercepted on stdin, parsed, and used to sync the parser's cursor to ground truth. Files: `state.rs`, `performer.rs`, `input.rs` (new `CursorPositionReport` variant), `proxy.rs` (send CPR in Task B, intercept in Task A).

---

### P1: Delay-Based Auto-Trigger (Debounce)

**Status:** DONE (2026-02-28)

Added a debounce mechanism via `tokio::sync::Notify` + a dedicated async task (Task D) in `proxy.rs`. When the shell reports a buffer change (OSC 7770) but no trigger char was pressed, Task B notifies the debounce task. The debounce task resets a timer on each new notification and fires `handler.trigger()` once the typing pause exceeds `delay_ms`.

Config: `delay_ms` in `[trigger]` section (default: 150ms, 0 to disable). Files changed: `gc-config/src/lib.rs`, `gc-pty/src/proxy.rs`.

Three trigger paths now coexist without double-firing:
- **Trigger chars** (space, `/`, `-`, `.`) → instant via Task B (existing)
- **Ctrl+Space** → instant via Task A (existing)
- **Any other typing + pause** → debounce fires after `delay_ms` silence (new)

---

### P2: P10k Instant Prompt Conflict

**Status:** DISSOLVED (2026-02-28) — switched from P10k to oh-my-zsh `half-life` theme

P10k has been fully removed from the user's shell setup. The `.zshrc` was cleaned up:
- Removed P10k instant prompt block
- Removed `source powerlevel10k.zsh-theme`
- Removed `source ~/.p10k.zsh`
- Set `ZSH_THEME="half-life"` (simple single-line lambda prompt with git status)

The `half-life` theme uses standard `precmd`/`preexec`/`chpwd` hooks, no async prompt rendering, no cursor gymnastics — fully compatible with Ghost Complete's PTY proxy. No code changes needed.

This also reduces the CPR cursor sync's workload — simpler prompts mean less drift between the VT parser's cursor tracking and reality.

---

### P3: Spec Loading Error Resilience

**Status:** DONE (2026-03-01)

Three improvements:

1. **Install-time validation upgraded** — `copy_specs_from()` now validates against `CompletionSpec` (full schema) instead of `serde_json::Value` (syntax-only). Valid JSON with wrong schema (e.g., missing `name` field) is now rejected at install time, not silently copied through.

2. **Runtime errors collected and summarized** — `SpecStore::load_from_dir()` returns a new `SpecLoadResult` struct containing both the store and a `Vec<String>` of error messages. `SuggestionEngine::new()` logs a single summary WARN (`"N spec(s) failed to load, run ghost-complete validate-specs for details"`) instead of N individual per-spec warnings.

3. **`ghost-complete validate-specs` subcommand** — loads config to find spec directories, validates every `.json` file against `CompletionSpec`, prints colored output with per-spec subcommand/option counts for valid specs and error details for invalid specs. Exits 0 if all valid, 1 if any failed.

**Files changed:** `gc-suggest/src/specs.rs` (SpecLoadResult, error collection), `gc-suggest/src/lib.rs` (re-export), `gc-suggest/src/engine.rs` (unpack result, summary log), `ghost-complete/Cargo.toml` (gc-suggest dep), `ghost-complete/src/install.rs` (CompletionSpec validation), `ghost-complete/src/validate.rs` (new), `ghost-complete/src/main.rs` (dispatch + help text)

---

### P4: Bash/Fish Shell Integration

**Status:** DONE (2026-03-01)

V1 implementation: Ctrl+Space manual trigger with buffer reporting via OSC 7770.

**Bash** (`shell/ghost-complete.bash`):
- `PROMPT_COMMAND` for OSC 133;A prompt marker
- DEBUG trap for OSC 133;C preexec marker
- `bind -x` binds Ctrl+Space to `_gc_report_buffer()` which sends `OSC 7770;cursor;buffer` using `READLINE_LINE`/`READLINE_POINT`
- Requires Bash 4.4+ for `bind -x` support

**Fish** (`shell/ghost-complete.fish`):
- `fish_prompt`/`fish_preexec` events for OSC 133;A/C markers
- `_gc_report_buffer` function uses `commandline` and `commandline -C` builtins
- Ctrl+Space bound to `_gc_report_buffer` via `bind \c@`

**Install:** `ghost-complete install` deploys all three shell scripts. Bash/fish users source the script manually (no auto-`.bashrc`/`config.fish` modification).

**Limitation:** Auto-trigger on typing not yet implemented for bash/fish. Only manual Ctrl+Space trigger works. Zsh has full auto-trigger via `line-pre-redraw` hook.

---

### P5: Keybinding Customization

**Status:** DONE (2026-03-01)

Six popup actions are now configurable via `[keybindings]` in `config.toml`:

| Action | Default | Description |
|--------|---------|-------------|
| `accept` | `"tab"` | Accept selected suggestion |
| `accept_and_enter` | `"enter"` | Accept + send Enter to shell |
| `dismiss` | `"escape"` | Dismiss popup |
| `navigate_up` | `"arrow_up"` | Move selection up |
| `navigate_down` | `"arrow_down"` | Move selection down |
| `trigger` | `"ctrl+space"` | Manual trigger when popup hidden |

Supported key names (case-insensitive): `tab`, `enter`, `escape`, `backspace`, `ctrl+space`, `arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`.

Invalid key names fail fast at startup (not silently at keypress time). Structural keys (printable chars, backspace, arrow left/right) remain non-configurable — they're fundamental to the PTY forwarding contract.

**Implementation details:**
- `gc-config`: `KeybindingsConfig` struct with serde `#[serde(default)]` — partial TOML overrides work.
- `gc-pty/handler.rs`: `Keybindings` struct holds resolved `KeyEvent` values. `parse_key_name()` converts strings to events. Dispatch refactored from hardcoded `match` to if-chain against `self.keybindings`, with structural keys still in a fallback `match`.
- `gc-pty/proxy.rs`: `Keybindings::from_config()` called at startup, passed to handler via builder chain.

**Also done (P5 follow-up):**
- `ghost-complete install` now writes a **default `config.toml`** with all settings commented out if one doesn't exist. Never clobbers existing configs. Every knob is discoverable without reading source code.
- **Config directory standardized to `~/.config/ghost-complete/`** — `dirs::config_dir()` returns `~/Library/Application Support/` on macOS which is wrong for CLI tools. Added `gc_config::config_dir()` helper that always returns `~/.config/ghost-complete/` via `dirs::home_dir()`. All four callsites (config loading, install, uninstall, spec auto-detection) updated.

**Files changed:** `gc-config/src/lib.rs`, `gc-pty/src/handler.rs`, `gc-pty/src/proxy.rs`, `ghost-complete/src/install.rs`, `ghost-complete/Cargo.toml`

**Operational note:** On macOS (Apple Silicon), `cp`-ing a binary over an existing one with running instances causes SIGKILL due to stale code signing cache. Fix: `codesign -f -s -` after copy, or use `cargo install --path`.

---

### P6: Color/Theme Customization

**Status:** DONE (2026-03-01)

Added `[theme]` section to config with `selected` and `description` style strings.

Style format: space-separated SGR attribute names. Supported: `reverse`, `dim`, `bold`, `underline`, `fg:N` (256-color foreground), `bg:N` (256-color background).

Examples:
```toml
[theme]
selected = "fg:255 bg:236 bold"
description = "dim"
```

**Implementation:**
- `gc-config`: `ThemeConfig` struct with `#[serde(default)]`, defaults to `reverse`/`dim`
- `gc-overlay`: `PopupTheme` struct holds precomputed ANSI byte sequences, `parse_style()` converts style strings to SGR sequences. Keeps gc-overlay independent of gc-config.
- `gc-pty`: `InputHandler.with_theme()` builder, `proxy.rs` builds `PopupTheme` from config at startup (fail-fast on invalid style strings)

**Files changed:** `gc-config/src/lib.rs`, `gc-overlay/src/render.rs`, `gc-overlay/src/lib.rs`, `gc-pty/src/handler.rs`, `gc-pty/src/proxy.rs`, `ghost-complete/src/install.rs`

---

## Architecture Notes for Future Reference

### How Triggers Work (Current Flow)

```
User types char
    → stdin forwarded to shell PTY (always)
    → if char ∈ trigger_chars: set trigger_requested = true
    → Task B reads shell output, forwards to terminal
    → if trigger_requested && buffer_dirty: compute suggestions
    → if suggestions non-empty: render popup
```

### How Popup Acceptance Works

```
User presses Tab while popup visible
    → get selected suggestion text
    → compute replacement (suggestion minus already-typed prefix)
    → write replacement bytes to shell PTY stdin
    → if suggestion ends with '/': set trigger_requested (directory chaining)
    → dismiss popup
```

### Spec Resolution Path

```
1. ~/.config/ghost-complete/specs/ (installed by `ghost-complete install`)
2. Next to binary (for cargo run / dev)
3. Current working directory /specs/
```

### Key Files for Each Priority Issue

| Issue | Primary Files |
|-------|--------------|
| P0: Templates | `crates/gc-suggest/src/specs.rs`, `crates/gc-suggest/src/engine.rs` |
| P1: Debounce | `crates/gc-pty/src/proxy.rs` (run_proxy event loop) |
| P2: P10k | N/A — dissolved, switched to half-life theme |
| P3: Resilience | `crates/gc-suggest/src/specs.rs`, `crates/gc-suggest/src/engine.rs`, `crates/ghost-complete/src/install.rs`, `crates/ghost-complete/src/validate.rs`, `crates/ghost-complete/src/main.rs` |
| P4: Bash/Fish | `shell/ghost-complete.bash`, `shell/ghost-complete.fish` |
| P5: Keybindings | `crates/gc-pty/src/handler.rs`, `crates/gc-config/src/lib.rs`, `crates/gc-pty/src/proxy.rs`, `crates/ghost-complete/src/install.rs` |
| P6: Themes | `crates/gc-overlay/src/lib.rs`, `crates/gc-config/src/lib.rs` |

---

## Implementation Order Recommendation

```
Phase A (Daily-Driver Quality):
  1. P0   — Template support in specs        ✅ DONE
  2. P0.5 — Popup cursor drift fix           ✅ DONE
  3. P1   — Delay-based auto-trigger         ✅ DONE
  4. P2   — P10k conflict                    ✅ DISSOLVED (switched to half-life theme)

Phase B (Polish):
  5. P5 — Keybinding customization           ✅ DONE (+ default config.toml, ~/.config/ standardization)
  6. P3 — Spec loading resilience            ✅ DONE (schema validation, SpecLoadResult, validate-specs subcommand)

Phase C (Distribution):
  7. P6 — Theme customization                ✅ DONE
  8. P4 — Bash/Fish integration              ✅ DONE (V1: Ctrl+Space trigger)
```

---

## Build & Deploy Notes

```bash
# Build + install (macOS Apple Silicon requires re-signing after cp)
cargo build --release \
  && cp target/release/ghost-complete ~/.cargo/bin/ \
  && codesign -f -s - ~/.cargo/bin/ghost-complete

# Then from a real terminal:
ghost-complete install

# Config lives at:
#   ~/.config/ghost-complete/config.toml   (user config, commented defaults)
#   ~/.config/ghost-complete/specs/        (completion specs)
#   ~/.config/ghost-complete/shell/        (shell integration scripts)
```

---

*Last updated: 2026-03-01 (Phase C complete — P4 + P6)*
