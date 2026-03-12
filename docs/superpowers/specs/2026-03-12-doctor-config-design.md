# Doctor & Config CLI Commands Design

## Goal

Add two diagnostic CLI commands — `ghost-complete doctor` (health check) and `ghost-complete config` (show resolved config) — to help users debug setup issues without reading source code.

## Architecture

Both commands are thin functions in the `ghost-complete` binary crate, dispatched from `main.rs` via the existing `shell_args.first()` match pattern. No new crates. Minimal dependency changes.

---

## `ghost-complete config`

### Behavior

Load config from `~/.config/ghost-complete/config.toml` (or `--config` path), merge with defaults, serialize as TOML, print to stdout.

If no config file exists, print the full default config. This doubles as documentation — users can pipe the output to create their config file.

### Output Format

```
$ ghost-complete config
[trigger]
auto_chars = [" ", "/", "-", "."]
delay_ms = 150

[popup]
max_visible = 10
min_width = 20
max_width = 60

[suggest]
max_results = 50
max_history_entries = 10000
generator_timeout_ms = 5000

[suggest.providers]
commands = true
history = true
filesystem = true
specs = true
git = true

[paths]
spec_dirs = []

[keybindings]
accept = "tab"
accept_and_enter = "enter"
dismiss = "escape"
navigate_up = "arrow_up"
navigate_down = "arrow_down"
trigger = "ctrl+/"

[theme]
selected = "reverse"
description = "dim"
```

### Implementation

**File:** `crates/ghost-complete/src/config_cmd.rs`

```rust
pub fn run_config(config_path: Option<&str>) -> Result<()> {
    let config = gc_config::GhostConfig::load(config_path)
        .context("failed to load config")?;
    let toml_str = toml::to_string_pretty(&config)
        .context("failed to serialize config")?;
    print!("{toml_str}");
    Ok(())
}
```

### Required Changes

1. **`gc-config/src/lib.rs`**: Add `Serialize` derive to `GhostConfig`, `TriggerConfig`, `PopupConfig`, `SuggestConfig`, `ProvidersConfig`, `PathsConfig`, `KeybindingsConfig`, `ThemeConfig`.
2. **`gc-config/Cargo.toml`**: Add `serde = { version = "1", features = ["derive"] }` if not already present (check if it comes transitively from `toml`).
3. **`ghost-complete/Cargo.toml`**: Move `toml = "1.0"` from `[dev-dependencies]` to `[dependencies]`.
4. **`ghost-complete/src/main.rs`**: Add `mod config_cmd;` and dispatch `"config"` in the match.

---

## `ghost-complete doctor`

### Behavior

Run 5 health checks. Print pass/fail results with colored indicators. Exit 0 if no FAILs, exit 1 if any FAILs.

### Severity Levels

| Level | Color | Meaning | Affects exit code |
|-------|-------|---------|-------------------|
| OK | Green `[OK]` | Check passed | No |
| WARN | Yellow `[WARN]` | Non-fatal issue, worth knowing | No |
| FAIL | Red `[FAIL]` | Will cause crashes or broken behavior | Yes (exit 1) |

### Checks

#### Check 1: Config file valid

- Attempt `GhostConfig::load()`.
- **OK**: Config parsed successfully, print path.
- **OK**: No config file found — "using defaults".
- **FAIL**: Config exists but fails to parse — print TOML error message.

#### Check 2: Keybinding names valid

- For each of the 6 keybinding fields (`accept`, `accept_and_enter`, `dismiss`, `navigate_up`, `navigate_down`, `trigger`), call `gc_pty::parse_key_name()`.
- **OK**: All 6 keybindings valid — "6 bindings".
- **FAIL**: Per invalid binding — `keybindings.<field> = "<value>" — <error>`.

Requires re-exporting `parse_key_name` from `gc-pty`. Currently the function is `pub` in `handler.rs` but the `handler` module is private. Add `pub use handler::parse_key_name;` to `gc-pty/src/lib.rs`.

#### Check 3: Theme style strings valid

- Call `gc_overlay::parse_style()` on `theme.selected` and `theme.description`.
- **OK**: Both styles valid.
- **FAIL**: Per invalid style — `theme.<field> = "<value>" — <error>`.

Requires re-exporting `parse_style` from `gc-pty` (which already depends on `gc-overlay`). Add `pub use gc_overlay::parse_style;` to `gc-pty/src/lib.rs`. This avoids adding `gc-overlay` as a direct dependency of `ghost-complete`.

#### Check 4: Shell integration installed

- Read `~/.zshrc` (or `$ZDOTDIR/.zshrc` if `ZDOTDIR` is set).
- Check for `# >>> ghost-complete initialize >>>` marker.
- **OK**: Block found.
- **WARN**: Block not found — "run `ghost-complete install` to set up shell integration". WARN not FAIL because bash/fish users won't have this, and proxy mode works without it.

#### Check 5: Running inside Ghostty

- Check `TERM_PROGRAM == "ghostty"` OR (`TMUX` is set AND `GHOSTTY_RESOURCES_DIR` is set).
- **OK**: Ghostty detected (report which: direct or tmux).
- **WARN**: Not Ghostty — "ghost-complete requires Ghostty for popup rendering". WARN not FAIL because user may be running doctor from a different terminal to diagnose.

### Output Format

```
Ghost Complete Doctor

  [OK]    Config file valid (~/.config/ghost-complete/config.toml)
  [OK]    Keybindings valid (6 bindings)
  [FAIL]  Theme style: unknown token "blink" in [theme] selected
  [OK]    Shell integration installed in ~/.zshrc
  [WARN]  Not running inside Ghostty (TERM_PROGRAM=alacritty)

1 issue found.
```

When all checks pass:
```
Ghost Complete Doctor

  [OK]  Config file valid (~/.config/ghost-complete/config.toml)
  [OK]  Keybindings valid (6 bindings)
  [OK]  Theme styles valid
  [OK]  Shell integration installed in ~/.zshrc
  [OK]  Running inside Ghostty

All checks passed.
```

### Implementation

**File:** `crates/ghost-complete/src/doctor.rs`

The function `run_doctor(config_path: Option<&str>) -> Result<()>` runs all 5 checks, accumulates results, prints the report, and exits with code 1 if any FAILs.

Each check is a helper function returning a `CheckResult`:

```rust
enum Severity {
    Ok,
    Warn,
    Fail,
}

struct CheckResult {
    name: &'static str,
    severity: Severity,
    message: String,
}
```

Check 1 (config) must run first — if it fails, checks 2 and 3 are skipped (they depend on valid config). Report them as `[SKIP] Keybindings — config invalid`.

### Required Changes

1. **`gc-pty/src/lib.rs`**: Add `pub use handler::parse_key_name;` and `pub use gc_overlay::parse_style;`.
2. **`ghost-complete/src/main.rs`**: Add `mod doctor;` and dispatch `"doctor"` in the match.
3. **`ghost-complete/src/main.rs`**: Update `after_help` string to include `doctor` and `config`.

---

## File Structure

```
crates/ghost-complete/src/
  main.rs          # Add "doctor" and "config" dispatch
  doctor.rs        # NEW — 5-check health diagnostic
  config_cmd.rs    # NEW — resolved config printer
crates/gc-config/src/
  lib.rs           # Add Serialize derive to all config structs
crates/gc-pty/src/
  lib.rs           # Re-export parse_key_name and parse_style
```

No new crates. No new external dependencies (toml is moved from dev-dep to dep in ghost-complete).

---

## Dependencies

### `ghost-complete/Cargo.toml`

Move `toml` from dev-dependencies to dependencies:

```toml
[dependencies]
toml = "1.0"
```

### `gc-config/Cargo.toml`

Ensure `serde` with `derive` feature is present (likely already is for `Deserialize`). No new dependencies.

---

## Testing

Both commands are I/O-light and use existing validated logic (`parse_key_name`, `parse_style`, `GhostConfig::load`). The existing unit tests for those functions cover correctness.

Integration-level testing for `doctor` and `config` is not worth the complexity — they're diagnostic tools that format and print existing validated data. Manual testing during implementation is sufficient.

---

## Scope

### In scope
- `ghost-complete config` command
- `ghost-complete doctor` command (5 checks)
- `Serialize` derive on all gc-config structs
- Re-export `parse_key_name` and `parse_style` from gc-pty
- Update `after_help` text in CLI

### Out of scope
- Unit tests for doctor/config (I/O formatting, not worth mocking)
- Additional doctor checks beyond the 5 defined
- JSON output format for doctor
- Config file generation (`ghost-complete config --init`)
