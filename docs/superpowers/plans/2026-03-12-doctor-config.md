# Doctor & Config CLI Commands Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `ghost-complete doctor` (5-check health diagnostic) and `ghost-complete config` (resolved config printer) CLI commands.

**Architecture:** Two new source files in the `ghost-complete` binary crate (`doctor.rs`, `config_cmd.rs`), dispatched from `main.rs` via the existing `shell_args.first()` match pattern. Requires adding `Serialize` derive to gc-config structs, re-exporting validators from gc-pty, and promoting `toml` to a regular dependency.

**Tech Stack:** Rust, serde (Serialize), toml, anyhow, gc-config, gc-pty, gc-overlay

---

## File Structure

```
crates/gc-config/src/lib.rs          — Add Serialize derive to all 8 config structs
crates/gc-pty/src/lib.rs             — Re-export parse_key_name and parse_style
crates/ghost-complete/Cargo.toml     — Move toml from dev-deps to deps
crates/ghost-complete/src/main.rs    — Add mod declarations + dispatch for "doctor" and "config"
crates/ghost-complete/src/config_cmd.rs  — NEW: resolved config printer
crates/ghost-complete/src/doctor.rs      — NEW: 5-check health diagnostic
```

---

## Chunk 1: Implementation

### Task 1: Add Serialize derive to gc-config structs

**Files:**
- Modify: `crates/gc-config/src/lib.rs:9,16,27,51,67,85,105,127,143`

- [ ] **Step 1: Add Serialize import**

In `crates/gc-config/src/lib.rs`, change line 9:
```rust
use serde::Deserialize;
```
to:
```rust
use serde::{Deserialize, Serialize};
```

- [ ] **Step 2: Add Serialize derive to all 8 structs**

Add `Serialize` to the derive macro of each struct. The structs and their current line numbers:

| Struct | Line | Current derives |
|--------|------|----------------|
| `GhostConfig` | 16 | `Debug, Clone, Default, Deserialize` |
| `KeybindingsConfig` | 27 | `Debug, Clone, Deserialize` |
| `TriggerConfig` | 51 | `Debug, Clone, Deserialize` |
| `PopupConfig` | 67 | `Debug, Clone, Deserialize` |
| `SuggestConfig` | 85 | `Debug, Clone, Deserialize` |
| `ProvidersConfig` | 105 | `Debug, Clone, Deserialize` |
| `ThemeConfig` | 127 | `Debug, Clone, Deserialize` |
| `PathsConfig` | 143 | `Debug, Clone, Default, Deserialize` |

Add `Serialize` after `Clone` in each derive. Example for `GhostConfig`:
```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
```

- [ ] **Step 3: Verify existing tests still pass**

Run: `cargo test -p gc-config`
Expected: All 7 tests pass. Adding `Serialize` doesn't affect deserialization behavior.

- [ ] **Step 4: Commit**

```bash
git add crates/gc-config/src/lib.rs
git commit -m "refactor: add Serialize derive to all gc-config structs"
```

---

### Task 2: Re-export parse_key_name and parse_style from gc-pty

**Files:**
- Modify: `crates/gc-pty/src/lib.rs:7,13`

- [ ] **Step 1: Add re-exports**

In `crates/gc-pty/src/lib.rs`, the current content is:
```rust
mod handler;
pub mod input;
mod proxy;
mod resize;
mod spawn;

pub use proxy::run_proxy;
```

Add two re-exports after the existing `pub use`:
```rust
mod handler;
pub mod input;
mod proxy;
mod resize;
mod spawn;

pub use proxy::run_proxy;
pub use handler::parse_key_name;
pub use gc_overlay::parse_style;
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p gc-pty && cargo check -p ghost-complete`
Expected: Both compile cleanly. `parse_key_name` is already `pub fn` in `handler.rs`, and `parse_style` is re-exported from `gc-overlay`'s `lib.rs`. The downstream `ghost-complete` check confirms the symbols are accessible as `gc_pty::parse_key_name` and `gc_pty::parse_style`.

- [ ] **Step 3: Commit**

```bash
git add crates/gc-pty/src/lib.rs
git commit -m "refactor: re-export parse_key_name and parse_style from gc-pty"
```

---

### Task 3: Promote toml to regular dependency in ghost-complete

**Files:**
- Modify: `crates/ghost-complete/Cargo.toml:19-34`

- [ ] **Step 1: Move toml from dev-dependencies to dependencies**

In `crates/ghost-complete/Cargo.toml`, add `toml = "1.0"` to `[dependencies]` (after line 29, alongside `serde_json`):
```toml
[dependencies]
gc-pty = { path = "../gc-pty" }
gc-config = { path = "../gc-config" }
gc-suggest = { path = "../gc-suggest" }
tokio = { workspace = true }
clap = { version = "4", features = ["derive"] }
anyhow = { workspace = true }
dirs = "6"
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde_json = "1"
toml = "1.0"
```

Remove `toml = "1.0"` from `[dev-dependencies]`:
```toml
[dev-dependencies]
portable-pty = "0.8"
tempfile = "3"
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p ghost-complete`
Expected: Compiles. `toml` was already resolved in `Cargo.lock` via `gc-config`.

- [ ] **Step 3: Commit**

```bash
git add crates/ghost-complete/Cargo.toml
git commit -m "chore: promote toml to regular dependency in ghost-complete"
```

---

### Task 4: Implement ghost-complete config command

**Files:**
- Create: `crates/ghost-complete/src/config_cmd.rs`
- Modify: `crates/ghost-complete/src/main.rs:1-3,14,78-95`

- [ ] **Step 1: Create config_cmd.rs**

Create `crates/ghost-complete/src/config_cmd.rs`:
```rust
use anyhow::{Context, Result};

pub fn run_config(config_path: Option<&str>) -> Result<()> {
    let config =
        gc_config::GhostConfig::load(config_path).context("failed to load config")?;
    let toml_str =
        toml::to_string_pretty(&config).context("failed to serialize config")?;
    println!("{toml_str}");
    Ok(())
}
```

- [ ] **Step 2: Add module declaration and dispatch in main.rs**

In `crates/ghost-complete/src/main.rs`, add `mod config_cmd;` at the top (after line 3):
```rust
mod install;
mod status;
mod validate;
mod config_cmd;
```

Add the `"config"` dispatch arm in the match block (after the `"status"` arm, before `_ => {}`):
```rust
        Some("config") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            return config_cmd::run_config(cli.config.as_deref());
        }
```

- [ ] **Step 3: Update after_help string**

In `main.rs` line 14, update the `after_help` string to include `config` and `doctor`:
```rust
    after_help = "COMMANDS:\n  install          Install shell integration (zsh/bash/fish)\n  uninstall        Remove shell integration\n  validate-specs   Validate completion spec files\n  status           Show loaded specs and JS compatibility\n  config           Show resolved configuration\n  doctor           Run health checks\n\nSHELL SUPPORT:\n  zsh   Full support (auto-installed into ~/.zshrc)\n  bash  Ctrl+Space trigger (source shell script from .bashrc)\n  fish  Ctrl+Space trigger (source shell script from config.fish)"
```

- [ ] **Step 4: Verify it compiles and runs**

Run: `cargo run -- config`
Expected: Prints the full default config as TOML to stdout. All sections present: `[trigger]`, `[popup]`, `[suggest]`, `[suggest.providers]`, `[paths]`, `[keybindings]`, `[theme]`.

- [ ] **Step 5: Run all tests**

Run: `cargo test -p ghost-complete`
Expected: All existing tests pass. No new tests needed for this command.

- [ ] **Step 6: Commit**

```bash
git add crates/ghost-complete/src/config_cmd.rs crates/ghost-complete/src/main.rs
git commit -m "feat: add ghost-complete config command"
```

---

### Task 5: Implement ghost-complete doctor command

**Files:**
- Create: `crates/ghost-complete/src/doctor.rs`
- Modify: `crates/ghost-complete/src/main.rs` (add mod + dispatch)

This is the largest task. The doctor command runs 5 checks and prints a colored report.

- [ ] **Step 1: Create doctor.rs with types and report printer**

Create `crates/ghost-complete/src/doctor.rs`:
```rust
use anyhow::{Context, Result};
use std::path::PathBuf;

enum Severity {
    Ok,
    Warn,
    Fail,
    Skip,
}

struct CheckResult {
    severity: Severity,
    message: String,
}

impl CheckResult {
    fn ok(msg: impl Into<String>) -> Self {
        Self { severity: Severity::Ok, message: msg.into() }
    }
    fn warn(msg: impl Into<String>) -> Self {
        Self { severity: Severity::Warn, message: msg.into() }
    }
    fn fail(msg: impl Into<String>) -> Self {
        Self { severity: Severity::Fail, message: msg.into() }
    }
    fn skip(msg: impl Into<String>) -> Self {
        Self { severity: Severity::Skip, message: msg.into() }
    }
}

fn print_results(results: &[CheckResult]) {
    println!("Ghost Complete Doctor\n");

    for result in results {
        let (label, color) = match result.severity {
            Severity::Ok   => ("[OK]  ", "\x1b[32m"),
            Severity::Warn => ("[WARN]", "\x1b[33m"),
            Severity::Fail => ("[FAIL]", "\x1b[31m"),
            Severity::Skip => ("[SKIP]", "\x1b[2m"),
        };
        println!("  {color}{label}\x1b[0m {}", result.message);
    }

    let fails = results.iter().filter(|r| matches!(r.severity, Severity::Fail)).count();
    let warns = results.iter().filter(|r| matches!(r.severity, Severity::Warn)).count();

    println!();
    if fails == 0 && warns == 0 {
        println!("All checks passed.");
    } else if fails == 0 {
        println!("{warns} warning(s).");
    } else {
        println!("{fails} issue(s) found.");
    }
}
```

- [ ] **Step 2: Implement check_config**

Add to `doctor.rs`:
```rust
fn check_config(config_path: Option<&str>) -> (CheckResult, Option<gc_config::GhostConfig>) {
    let path = match config_path {
        Some(p) => PathBuf::from(p),
        None => gc_config::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("config.toml"),
    };

    if !path.exists() {
        // No config file is fine — defaults are used
        return (
            CheckResult::ok("Config file: using defaults (no config.toml found)"),
            Some(gc_config::GhostConfig::default()),
        );
    }

    match gc_config::GhostConfig::load(config_path) {
        Ok(config) => (
            CheckResult::ok(format!("Config file valid ({})", path.display())),
            Some(config),
        ),
        Err(e) => (
            CheckResult::fail(format!("Config file invalid ({}): {e}", path.display())),
            None,
        ),
    }
}
```

- [ ] **Step 3: Implement check_keybindings**

Add to `doctor.rs`:
```rust
fn check_keybindings(config: &gc_config::GhostConfig) -> CheckResult {
    let bindings = [
        ("accept", &config.keybindings.accept),
        ("accept_and_enter", &config.keybindings.accept_and_enter),
        ("dismiss", &config.keybindings.dismiss),
        ("navigate_up", &config.keybindings.navigate_up),
        ("navigate_down", &config.keybindings.navigate_down),
        ("trigger", &config.keybindings.trigger),
    ];

    let mut errors = Vec::new();
    for (name, value) in &bindings {
        if let Err(e) = gc_pty::parse_key_name(value) {
            errors.push(format!("keybindings.{name} = \"{value}\" — {e}"));
        }
    }

    if errors.is_empty() {
        CheckResult::ok(format!("Keybindings valid ({} bindings)", bindings.len()))
    } else {
        CheckResult::fail(format!("Keybindings invalid: {}", errors.join("; ")))
    }
}
```

- [ ] **Step 4: Implement check_theme**

Add to `doctor.rs`:
```rust
fn check_theme(config: &gc_config::GhostConfig) -> CheckResult {
    let styles = [
        ("selected", &config.theme.selected),
        ("description", &config.theme.description),
    ];

    let mut errors = Vec::new();
    for (name, value) in &styles {
        if let Err(e) = gc_pty::parse_style(value) {
            errors.push(format!("[theme] {name} = \"{value}\" — {e}"));
        }
    }

    if errors.is_empty() {
        CheckResult::ok("Theme styles valid")
    } else {
        CheckResult::fail(format!("Theme style: {}", errors.join("; ")))
    }
}
```

- [ ] **Step 5: Implement check_shell_integration**

Add to `doctor.rs`:
```rust
fn check_shell_integration() -> CheckResult {
    let zshrc = dirs::home_dir().map(|h| h.join(".zshrc"));

    let Some(path) = zshrc else {
        return CheckResult::warn("Cannot determine home directory");
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            if content.contains("# >>> ghost-complete initialize >>>") {
                CheckResult::ok(format!("Shell integration installed in {}", path.display()))
            } else {
                CheckResult::warn(
                    "Shell integration not found in ~/.zshrc — run `ghost-complete install`",
                )
            }
        }
        Err(_) => CheckResult::warn("~/.zshrc not found — run `ghost-complete install`"),
    }
}
```

- [ ] **Step 6: Implement check_ghostty**

Add to `doctor.rs`:
```rust
fn check_ghostty() -> CheckResult {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let in_tmux = std::env::var("TMUX").is_ok();
    let has_ghostty_resources = std::env::var("GHOSTTY_RESOURCES_DIR").is_ok();

    if term_program.eq_ignore_ascii_case("ghostty") {
        CheckResult::ok("Running inside Ghostty")
    } else if in_tmux && has_ghostty_resources {
        CheckResult::ok("Running inside Ghostty (via tmux)")
    } else {
        let actual = if term_program.is_empty() {
            "unknown".to_string()
        } else {
            term_program
        };
        CheckResult::warn(format!(
            "Not running inside Ghostty (TERM_PROGRAM={actual})"
        ))
    }
}
```

- [ ] **Step 7: Implement run_doctor that orchestrates all checks**

Add to `doctor.rs`:
```rust
pub fn run_doctor(config_path: Option<&str>) -> Result<()> {
    let mut results = Vec::new();

    // Check 1: Config file
    let (config_result, config) = check_config(config_path);
    results.push(config_result);

    // Checks 2 & 3 depend on valid config
    match &config {
        Some(cfg) => {
            results.push(check_keybindings(cfg));
            results.push(check_theme(cfg));
        }
        None => {
            results.push(CheckResult::skip("Keybindings — config invalid"));
            results.push(CheckResult::skip("Theme styles — config invalid"));
        }
    }

    // Check 4: Shell integration
    results.push(check_shell_integration());

    // Check 5: Ghostty
    results.push(check_ghostty());

    print_results(&results);

    let has_fails = results.iter().any(|r| matches!(r.severity, Severity::Fail));
    if has_fails {
        std::process::exit(1);
    }

    Ok(())
}
```

- [ ] **Step 8: Add module declaration and dispatch in main.rs**

In `crates/ghost-complete/src/main.rs`, add `mod doctor;` at the top:
```rust
mod install;
mod status;
mod validate;
mod config_cmd;
mod doctor;
```

Add the `"doctor"` dispatch arm in the match block (after `"config"`):
```rust
        Some("doctor") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            return doctor::run_doctor(cli.config.as_deref());
        }
```

- [ ] **Step 9: Verify compilation**

Run: `cargo check -p ghost-complete`
Expected: Compiles cleanly.

- [ ] **Step 10: Manual test — all checks pass**

Run: `cargo run -- doctor`
Expected output (approximately):
```
Ghost Complete Doctor

  [OK]   Config file valid (~/.config/ghost-complete/config.toml)
  [OK]   Keybindings valid (6 bindings)
  [OK]   Theme styles valid
  [OK]   Shell integration installed in /Users/<you>/.zshrc
  [WARN] Not running inside Ghostty (TERM_PROGRAM=<your terminal>)

1 warning(s).
```

The WARN for Ghostty is expected when running from a non-Ghostty terminal.

- [ ] **Step 11: Manual test — bad config**

Create a temp bad config and test:
```bash
echo "broken [toml = {{{" > /tmp/bad-config.toml
cargo run -- --config /tmp/bad-config.toml doctor
```
Expected: FAIL for config, SKIP for keybindings and theme, then normal shell/ghostty checks. Exit code 1.

- [ ] **Step 12: Run full test suite**

Run: `cargo test`
Expected: All workspace tests pass.

Run: `cargo clippy --all-targets`
Expected: No warnings.

- [ ] **Step 13: Commit**

```bash
git add crates/ghost-complete/src/doctor.rs crates/ghost-complete/src/main.rs
git commit -m "feat: add ghost-complete doctor command"
```
