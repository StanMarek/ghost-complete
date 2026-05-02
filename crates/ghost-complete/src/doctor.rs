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

fn render_results<W: std::io::Write>(results: &[CheckResult], out: &mut W) -> std::io::Result<()> {
    writeln!(out, "Ghost Complete Doctor\n")?;

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
        writeln!(
            out,
            "  {color}{label}\x1b[0m {}",
            sanitize_for_terminal(&result.message)
        )?;
    }

    let fails = results
        .iter()
        .filter(|r| matches!(r.severity, Severity::Fail))
        .count();
    let warns = results
        .iter()
        .filter(|r| matches!(r.severity, Severity::Warn))
        .count();

    writeln!(out)?;
    if fails == 0 && warns == 0 {
        writeln!(out, "All checks passed.")?;
    } else if fails == 0 {
        writeln!(out, "{warns} warning(s).")?;
    } else {
        writeln!(out, "{fails} issue(s) found.")?;
    }
    Ok(())
}

fn print_results(results: &[CheckResult]) {
    let _ = render_results(results, &mut std::io::stdout().lock());
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
        ("feedback_loading", &resolved.feedback_loading),
        ("feedback_empty", &resolved.feedback_empty),
        ("feedback_error", &resolved.feedback_error),
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
/// the PTY proxy does at startup, then reports the spec count. Catches the
/// "binary works, but autocomplete is empty" failure mode where neither
/// `~/.config/ghost-complete/specs` nor a sibling `specs/` dir exists and
/// the embedded fallback fails to materialize.
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
        // Loud FAIL so a user running `doctor` after a fresh `cargo install`
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

/// Count generators on a single spec that carry a `_corrected_in` marker.
/// Walks args, options, and the full subcommand tree iteratively to avoid
/// re-introducing the recursion-depth attack surface removed from the other
/// spec walkers.
fn count_corrected_generators_in_spec(spec: &gc_suggest::CompletionSpec) -> usize {
    use gc_suggest::specs::{ArgSpec, OptionSpec, SubcommandSpec};

    fn count_in_args(args: &[ArgSpec]) -> usize {
        args.iter()
            .flat_map(|a| a.generators.iter())
            .filter(|g| g.corrected_in.is_some())
            .count()
    }

    fn count_in_options(options: &[OptionSpec]) -> usize {
        options
            .iter()
            .filter_map(|o| o.args.as_ref())
            .flat_map(|a| a.generators.iter())
            .filter(|g| g.corrected_in.is_some())
            .count()
    }

    let mut total = count_in_args(&spec.args) + count_in_options(&spec.options);

    let mut stack: Vec<&SubcommandSpec> = spec.subcommands.iter().collect();
    while let Some(sub) = stack.pop() {
        total += count_in_args(&sub.args);
        total += count_in_options(&sub.options);
        stack.extend(sub.subcommands.iter());
    }

    total
}

/// Check 7: Corrected generators. Walks the loaded SpecStore and counts
/// generators whose prior conversion was mis-lowered and has since been
/// corrected (see CHANGELOG.md's "Corrected" sections and the
/// `_corrected_in` lifecycle in docs/SPECS.md). If any are found, emits a
/// Warn result so users who upgrade see _why_ some completions changed
/// behaviour.
///
/// Re-loads specs with the same resolver `check_specs` uses — cheaper than
/// plumbing the store out of Check 6, and keeps the two checks independent
/// (a broken spec dir still produces a skip here rather than a hard fail).
fn check_corrections(config: &gc_config::GhostConfig) -> CheckResult {
    let dirs = gc_suggest::spec_dirs::resolve_spec_dirs(&config.paths.spec_dirs);
    let result = match gc_suggest::SpecStore::load_from_dirs(&dirs) {
        Ok(r) => r,
        // Spec load already failed in check_specs — no point duplicating the
        // failure; skip so the doctor output stays readable.
        Err(_) => {
            return CheckResult::skip(
                "Corrected generators — spec load failed (see Completion specs check)",
            );
        }
    };

    check_corrections_for_store(&result.store)
}

/// Pure accounting logic — separated from directory resolution so it can be
/// unit-tested against an in-memory `SpecStore`.
fn check_corrections_for_store(store: &gc_suggest::SpecStore) -> CheckResult {
    let mut affected_specs: Vec<(&str, usize)> = store
        .iter()
        .filter_map(|(name, spec)| {
            let n = count_corrected_generators_in_spec(spec);
            if n == 0 {
                None
            } else {
                Some((name, n))
            }
        })
        .collect();

    if affected_specs.is_empty() {
        return CheckResult::ok("No corrected-generator warnings");
    }

    // Stable, alphabetical spec ordering so repeated runs produce identical
    // messages (useful for diffing doctor output across CI runs).
    affected_specs.sort_by_key(|(name, _)| *name);

    let total_generators: usize = affected_specs.iter().map(|(_, n)| *n).sum();
    let spec_count = affected_specs.len();
    const PREVIEW_LIMIT: usize = 5;

    let preview: Vec<&str> = affected_specs
        .iter()
        .take(PREVIEW_LIMIT)
        .map(|(name, _)| *name)
        .collect();
    let preview_str = preview.join(", ");
    let tail = if spec_count > PREVIEW_LIMIT {
        format!(", ...and {} more", spec_count - PREVIEW_LIMIT)
    } else {
        String::new()
    };

    CheckResult::warn(format!(
        "Warning: {total_generators} generator(s) across {spec_count} spec(s) were \
         previously returning incorrect completions and are now disabled pending \
         proper handling. See CHANGELOG. Affected specs: {preview_str}{tail}"
    ))
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
    // Without this check, doctor would report a healthy install while the
    // proxy silently ran with zero specs.
    match &config {
        Some(cfg) => results.push(check_specs(cfg)),
        None => results.push(CheckResult::skip(
            "Completion specs — config invalid, cannot resolve spec dirs",
        )),
    }

    // Check 7: Corrected generators. Surfaces generators whose prior
    // conversion was mis-lowered and has since been corrected so users who
    // upgrade see _why_ some previously-working completions are now
    // requires_js until a JS runtime lands. Skip if config invalid (same
    // dependency rule as Check 6).
    match &config {
        Some(cfg) => results.push(check_corrections(cfg)),
        None => results.push(CheckResult::skip(
            "Corrected generators — config invalid, cannot resolve spec dirs",
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
    /// zero specs loaded.
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

    /// Build a `SpecStore` by writing fixtures to a temp directory and
    /// loading them via the normal loader. Keeps the test honest — exercises
    /// the same deserialization path real specs go through.
    fn store_from_json_fixtures(
        fixtures: &[(&str, &str)],
    ) -> (gc_suggest::SpecStore, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        for (file, body) in fixtures {
            std::fs::write(dir.path().join(file), body).unwrap();
        }
        let result = gc_suggest::SpecStore::load_from_dirs(&[dir.path().to_path_buf()]).unwrap();
        assert!(
            result.errors.is_empty(),
            "fixture load errors: {:?}",
            result.errors
        );
        (result.store, dir)
    }

    #[test]
    fn check_corrections_for_store_reports_ok_when_none() {
        // A store with one spec whose generators have no _corrected_in must
        // produce an OK result.
        let (store, _dir) = store_from_json_fixtures(&[(
            "clean.json",
            r#"{
                "name": "clean",
                "args": [{
                    "name": "target",
                    "generators": [{"type": "git_branches"}]
                }]
            }"#,
        )]);
        let result = check_corrections_for_store(&store);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(
            result.message.contains("No corrected-generator"),
            "unexpected message: {}",
            result.message
        );
    }

    #[test]
    fn check_corrections_for_store_warns_when_present() {
        // One generator with _corrected_in, one without, in the same spec.
        // Accounting must count exactly one.
        let (store, _dir) = store_from_json_fixtures(&[(
            "affected.json",
            r#"{
                "name": "affected",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "git_branches"},
                        {"requires_js": true, "js_source": "fn", "_corrected_in": "v0.10.0"}
                    ]
                }]
            }"#,
        )]);
        let result = check_corrections_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Warn),
            "expected Warn, got message: {}",
            result.message
        );
        assert!(
            result.message.contains("affected"),
            "message must name the affected spec: {}",
            result.message
        );
        assert!(
            result.message.contains("1 generator(s)"),
            "message must count generators: {}",
            result.message
        );
        assert!(
            result.message.contains("1 spec(s)"),
            "message must count specs: {}",
            result.message
        );
        assert!(
            result.message.contains("CHANGELOG"),
            "message must direct user to CHANGELOG: {}",
            result.message
        );
    }

    #[test]
    fn check_corrections_for_store_truncates_to_five_with_suffix() {
        // Seven affected specs — first five listed, rest summarized as
        // "...and 2 more". Alphabetical ordering for stable output.
        let fixtures: Vec<(String, String)> = (b'a'..=b'g')
            .map(|ch| {
                let name = format!("spec-{}", ch as char);
                let body = format!(
                    r#"{{
                        "name": "{name}",
                        "args": [{{
                            "name": "t",
                            "generators": [
                                {{"requires_js": true, "js_source": "fn", "_corrected_in": "v0.10.0"}}
                            ]
                        }}]
                    }}"#
                );
                (format!("{name}.json"), body)
            })
            .collect();
        let refs: Vec<(&str, &str)> = fixtures
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (store, _dir) = store_from_json_fixtures(&refs);

        let result = check_corrections_for_store(&store);
        assert!(matches!(result.severity, Severity::Warn));
        // Alphabetical: a..e in preview, f and g summarized.
        assert!(result.message.contains("spec-a"));
        assert!(result.message.contains("spec-e"));
        assert!(
            result.message.contains("...and 2 more"),
            "expected truncation suffix, got: {}",
            result.message
        );
        // Later specs must NOT appear verbatim in the preview.
        assert!(
            !result.message.contains("spec-f"),
            "spec-f should be truncated: {}",
            result.message
        );
        // Totals: 7 generators across 7 specs.
        assert!(
            result.message.contains("7 generator(s) across 7 spec(s)"),
            "bad totals in message: {}",
            result.message
        );
    }

    #[test]
    fn count_corrected_generators_walks_nested_subcommands_and_option_args() {
        // Generators live in all three positions — top-level args, option
        // args, and nested subcommand args. All three must be counted.
        let spec: gc_suggest::CompletionSpec = serde_json::from_str(
            r#"{
                "name": "tree",
                "args": [{
                    "name": "root",
                    "generators": [
                        {"requires_js": true, "_corrected_in": "v0.10.0"}
                    ]
                }],
                "options": [{
                    "name": ["-f"],
                    "args": {
                        "name": "val",
                        "generators": [
                            {"requires_js": true, "_corrected_in": "v0.10.0"}
                        ]
                    }
                }],
                "subcommands": [{
                    "name": "nested",
                    "subcommands": [{
                        "name": "deeper",
                        "args": [{
                            "name": "leaf",
                            "generators": [
                                {"requires_js": true, "_corrected_in": "v0.10.0"},
                                {"type": "git_branches"}
                            ]
                        }]
                    }]
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(count_corrected_generators_in_spec(&spec), 3);
    }

    #[test]
    fn doctor_renders_sanitize_hostile_message() {
        let results = vec![CheckResult {
            severity: Severity::Fail,
            message: "\x1b[31mboom\x07nul\x00".to_string(),
        }];
        let mut buf = Vec::new();
        render_results(&results, &mut buf).unwrap();
        let emitted = String::from_utf8(buf).unwrap();

        let (_prefix, body) = emitted.split_once("[FAIL]\x1b[0m ").expect(
            "render output must contain the [FAIL] label with reset; \
             body starts after that: {emitted:?}",
        );
        let line_end = body.find('\n').unwrap_or(body.len());
        let rendered_message = &body[..line_end];

        assert!(
            !rendered_message.contains('\x1b'),
            "rendered message must not contain ESC bytes: {rendered_message:?}"
        );
        assert!(
            !rendered_message.contains('\x07'),
            "rendered message must not contain BEL bytes: {rendered_message:?}"
        );
        assert!(
            !rendered_message.contains('\x00'),
            "rendered message must not contain NUL bytes: {rendered_message:?}"
        );
    }
}
