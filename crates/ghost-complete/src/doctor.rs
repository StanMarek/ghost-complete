use anyhow::Result;
use std::path::PathBuf;

use crate::sanitize::sanitize_for_terminal;

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
        Self {
            severity: Severity::Ok,
            message: msg.into(),
        }
    }
    fn warn(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            message: msg.into(),
        }
    }
    fn fail(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Fail,
            message: msg.into(),
        }
    }
    fn skip(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Skip,
            message: msg.into(),
        }
    }
}

fn print_results(results: &[CheckResult]) {
    println!("Ghost Complete Doctor\n");

    for result in results {
        let (label, color) = match result.severity {
            Severity::Ok => ("[OK]  ", "\x1b[32m"),
            Severity::Warn => ("[WARN]", "\x1b[33m"),
            Severity::Fail => ("[FAIL]", "\x1b[31m"),
            Severity::Skip => ("[SKIP]", "\x1b[2m"),
        };
        // Messages are composed from attacker-controllable inputs: config
        // spec dirs, keybinding/theme values, shell paths, terminal display
        // strings, OS error text. Strip control chars at the print boundary
        // so a hostile `~/.config/ghost-complete/config.toml` can't smuggle
        // CSI/OSC sequences through `ghost-complete doctor` output.
        println!(
            "  {color}{label}\x1b[0m {}",
            sanitize_for_terminal(&result.message)
        );
    }

    let fails = results
        .iter()
        .filter(|r| matches!(r.severity, Severity::Fail))
        .count();
    let warns = results
        .iter()
        .filter(|r| matches!(r.severity, Severity::Warn))
        .count();

    println!();
    if fails == 0 && warns == 0 {
        println!("All checks passed.");
    } else if fails == 0 {
        println!("{warns} warning(s).");
    } else {
        println!("{fails} issue(s) found.");
    }
}

/// Check 1: Config file valid
fn check_config(config_path: Option<&str>) -> (CheckResult, Option<gc_config::GhostConfig>) {
    let path = match config_path {
        Some(p) => PathBuf::from(p),
        None => {
            let Some(dir) = gc_config::config_dir() else {
                // HOME unset — refuse to probe CWD for config.
                return (
                    CheckResult::warn("Config file: HOME unset, using defaults"),
                    Some(gc_config::GhostConfig::default()),
                );
            };
            dir.join("config.toml")
        }
    };

    if !path.exists() {
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

/// Check 2: Keybinding names valid
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

/// Check 3: Theme style strings valid
fn check_theme(config: &gc_config::GhostConfig) -> CheckResult {
    let resolved = match config.theme.resolve() {
        Ok(t) => t,
        Err(e) => return CheckResult::fail(format!("Theme preset: {e}")),
    };

    let styles = [
        ("selected", &resolved.selected),
        ("description", &resolved.description),
        ("match_highlight", &resolved.match_highlight),
        ("item_text", &resolved.item_text),
        ("scrollbar", &resolved.scrollbar),
        ("border", &resolved.border),
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

/// Check 4: Shell integration installed in ~/.zshrc
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
        Err(e) => CheckResult::warn(format!("Cannot read ~/.zshrc: {e}")),
    }
}

/// Check 5: Running inside a supported terminal
///
/// Uses `TerminalProfile::detect()` as the single source of truth for which
/// terminal is running, avoiding divergence between detect() and is_supported().
fn check_terminal(config: &gc_config::GhostConfig) -> CheckResult {
    let profile = gc_terminal::TerminalProfile::detect();
    check_terminal_profile(&profile, config.experimental.multi_terminal)
}

/// Check 6: Completion specs actually load.
///
/// Resolves spec dirs and calls `SpecStore::load_from_dirs` exactly the way
/// the PTY proxy does at startup, then reports the spec count. This is the
/// only doctor check that catches the "binary works, but autocomplete is
/// empty" failure mode that the 2026-04-17 audit found — the proxy path
/// silently returned zero specs whenever neither `~/.config/ghost-complete/specs`
/// nor a sibling `specs/` dir existed and the embedded fallback hadn't been
/// wired up.
fn check_specs(config: &gc_config::GhostConfig) -> CheckResult {
    let dirs = gc_suggest::spec_dirs::resolve_spec_dirs(&config.paths.spec_dirs);
    let dir_count = dirs.len();

    let result = match gc_suggest::SpecStore::load_from_dirs(&dirs) {
        Ok(r) => r,
        Err(e) => return CheckResult::fail(format!("Spec load failed: {e}")),
    };

    let loaded = result.store.len();
    let dir_summary = dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    if loaded == 0 {
        // This is the bug the audit caught: zero specs and no error. Loud
        // FAIL so a user running `doctor` after a fresh `cargo install`
        // gets an actionable signal instead of silently degraded
        // autocomplete.
        return CheckResult::fail(format!(
            "Completion specs: 0 loaded from {dir_count} directory(ies) \
             [{dir_summary}] — autocomplete will be missing all per-command \
             completions. Run `ghost-complete install` to deploy the \
             bundled spec set."
        ));
    }

    let mut msg = format!(
        "Completion specs: {loaded} loaded from {dir_count} directory(ies) \
         [{dir_summary}]"
    );
    if !result.errors.is_empty() {
        msg.push_str(&format!(
            " ({} spec(s) failed to parse — run `ghost-complete \
             validate-specs` for details)",
            result.errors.len()
        ));
        return CheckResult::warn(msg);
    }
    CheckResult::ok(msg)
}

/// Testable terminal check logic — pure function on profile.
fn check_terminal_profile(
    profile: &gc_terminal::TerminalProfile,
    multi_terminal: bool,
) -> CheckResult {
    if !profile.terminal().is_known() {
        if multi_terminal {
            return CheckResult::ok(format!(
                "Unknown terminal ({}) — multi_terminal enabled, proceeding anyway",
                profile.display_name(),
            ));
        }
        return CheckResult::warn(format!(
            "Unsupported terminal ({}) — supported: {}",
            profile.display_name(),
            gc_terminal::Terminal::supported_terminals().join(", ")
        ));
    }

    let msg = format!(
        "Running inside {} (render: {}, prompt: {})",
        profile.display_name(),
        profile.render_strategy(),
        profile.prompt_detection()
    );

    CheckResult::ok(msg)
}

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

    // Check 5: Terminal support (needs config for experimental flag)
    match &config {
        Some(cfg) => results.push(check_terminal(cfg)),
        None => results.push(CheckResult::skip(
            "Terminal support — config invalid, cannot check experimental flags",
        )),
    }

    // Check 6: Completion specs load via the same path the PTY proxy uses.
    // Without this check, doctor reported a healthy install while the proxy
    // silently ran with zero specs (the 2026-04-17 audit's CRITICAL bug).
    match &config {
        Some(cfg) => results.push(check_specs(cfg)),
        None => results.push(CheckResult::skip(
            "Completion specs — config invalid, cannot resolve spec dirs",
        )),
    }

    print_results(&results);

    let has_fails = results.iter().any(|r| matches!(r.severity, Severity::Fail));
    if has_fails {
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_terminal_ghostty_ok() {
        let profile = gc_terminal::TerminalProfile::for_ghostty();
        let result = check_terminal_profile(&profile, false);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("Ghostty"));
    }

    #[test]
    fn test_check_terminal_kitty_ok() {
        let profile = gc_terminal::TerminalProfile::for_kitty();
        let result = check_terminal_profile(&profile, false);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("Kitty"));
    }

    #[test]
    fn test_check_terminal_wezterm_ok() {
        let profile = gc_terminal::TerminalProfile::for_wezterm();
        let result = check_terminal_profile(&profile, false);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("WezTerm"));
    }

    #[test]
    fn test_check_terminal_alacritty_ok() {
        let profile = gc_terminal::TerminalProfile::for_alacritty();
        let result = check_terminal_profile(&profile, false);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("Alacritty"));
    }

    #[test]
    fn test_check_terminal_rio_ok() {
        let profile = gc_terminal::TerminalProfile::for_rio();
        let result = check_terminal_profile(&profile, false);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("Rio"));
    }

    #[test]
    fn test_check_terminal_iterm2_ok() {
        let profile = gc_terminal::TerminalProfile::for_iterm2();
        let result = check_terminal_profile(&profile, false);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("iTerm2"));
    }

    #[test]
    fn test_check_terminal_unknown_warns() {
        let profile = gc_terminal::TerminalProfile::for_unknown("foot");
        let result = check_terminal_profile(&profile, false);
        assert!(matches!(result.severity, Severity::Warn));
        assert!(result.message.contains("Unsupported"));
    }

    #[test]
    fn test_check_terminal_unknown_with_multi_terminal_ok() {
        let profile = gc_terminal::TerminalProfile::for_unknown("foot");
        let result = check_terminal_profile(&profile, true);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("multi_terminal"));
    }

    /// Pin the user-facing spec health check to the embedded fallback path.
    ///
    /// `check_specs` calls `resolve_spec_dirs` + `SpecStore::load_from_dirs`
    /// — the same chain the PTY proxy uses — and must never report OK with
    /// zero specs loaded. Before the 2026-04-17 audit fix the proxy could
    /// silently start with an empty `SpecStore` whenever the on-disk
    /// auto-detection chain bottomed out; the doctor command happily
    /// reported the install as healthy.
    ///
    /// We can't directly stub the resolver's environment lookups in this
    /// process, but we *can* assert that with a default config the check
    /// resolves at least one spec dir and loads at least one spec — which
    /// implicitly proves that either an on-disk dir was found or the
    /// embedded fallback materialized a usable one.
    #[test]
    fn check_specs_loads_non_empty_with_default_config() {
        let config = gc_config::GhostConfig::default();
        let result = check_specs(&config);
        assert!(
            !matches!(result.severity, Severity::Fail),
            "check_specs failed with default config — message: {}",
            result.message
        );
        // The OK / WARN message format always includes a "Completion specs: \
        // <N> loaded" prefix when at least one spec was loaded.
        assert!(
            result.message.starts_with("Completion specs:"),
            "unexpected message shape: {}",
            result.message
        );
        assert!(
            !result.message.starts_with("Completion specs: 0 loaded"),
            "check_specs reported 0 specs loaded — embedded fallback is \
             not wired up: {}",
            result.message
        );
    }
}
