use anyhow::Result;
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
        println!("  {color}{label}\x1b[0m {}", result.message);
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
        None => gc_config::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("config.toml"),
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
/// Detects the terminal using gc_terminal and reports the detected profile
/// including render strategy and prompt detection method.
fn check_terminal() -> CheckResult {
    let profile = gc_terminal::TerminalProfile::detect();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

    if gc_terminal::is_supported(&term_program) {
        CheckResult::ok(format!(
            "Running inside {} (render: {}, prompt: {})",
            profile.name, profile.render_strategy, profile.prompt_detection
        ))
    } else if std::env::var("TMUX").is_ok() {
        // In tmux, check if we detected a known terminal via env vars
        if !matches!(profile.terminal, gc_terminal::Terminal::Unknown(_)) {
            CheckResult::ok(format!(
                "Running inside {} (render: {}, prompt: {})",
                profile.name, profile.render_strategy, profile.prompt_detection
            ))
        } else {
            CheckResult::warn(format!(
                "Unsupported terminal in tmux (TERM_PROGRAM={}) — supported: {}",
                if term_program.is_empty() {
                    "unknown"
                } else {
                    &term_program
                },
                gc_terminal::SUPPORTED_TERMINALS.join(", ")
            ))
        }
    } else {
        CheckResult::warn(format!(
            "Unsupported terminal (TERM_PROGRAM={}) — supported: {}",
            if term_program.is_empty() {
                "unknown"
            } else {
                &term_program
            },
            gc_terminal::SUPPORTED_TERMINALS.join(", ")
        ))
    }
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

    // Check 5: Terminal support
    results.push(check_terminal());

    print_results(&results);

    let has_fails = results.iter().any(|r| matches!(r.severity, Severity::Fail));
    if has_fails {
        std::process::exit(1);
    }

    Ok(())
}
