use anyhow::Result;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use gc_suggest::specs::{
    AliasConflict, AliasConflictDisposition, AliasConflictKind, ArgSpec, CompletionSpec,
    GeneratorSpec, JsRuntimeKind, OptionSpec, SubcommandSpec,
};

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

fn load_specs_for_resolution(
    dirs: &[PathBuf],
    include_embedded: bool,
) -> Result<gc_suggest::SpecLoadResult> {
    if include_embedded {
        gc_suggest::SpecStore::load_with_embedded(dirs)
    } else {
        gc_suggest::SpecStore::load_from_dirs(dirs)
    }
}

fn load_specs_for_config(config: &gc_config::GhostConfig) -> Result<gc_suggest::SpecLoadResult> {
    let resolution =
        gc_suggest::spec_dirs::resolve_spec_dirs_with_provenance(&config.paths.spec_dirs);
    load_specs_for_resolution(&resolution.dirs, resolution.include_embedded)
}

/// Check 6: Completion specs actually load.
///
/// Resolves spec dirs with runtime provenance and uses the same embedded
/// fallback policy as the PTY proxy, then reports the resolved runtime spec
/// count. Catches the "binary works, but autocomplete is empty" failure mode
/// where neither filesystem specs nor the embedded fallback produce usable
/// completions.
fn check_specs(config: &gc_config::GhostConfig) -> CheckResult {
    let resolution =
        gc_suggest::spec_dirs::resolve_spec_dirs_with_provenance(&config.paths.spec_dirs);
    check_specs_for_resolution(&resolution.dirs, resolution.include_embedded)
}

fn check_specs_for_resolution(dirs: &[PathBuf], include_embedded: bool) -> CheckResult {
    let result = match load_specs_for_resolution(dirs, include_embedded) {
        Ok(r) => r,
        Err(e) => return CheckResult::fail(format!("Spec load failed: {e}")),
    };

    let loaded = result.store.iter().count();
    let mut sources = dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>();
    if include_embedded {
        sources.push("<embedded>".to_string());
    }
    let source_count = sources.len();
    let source_summary = sources.join(", ");
    let lazy_errors = result.store.force_load_errors();

    let mut issue_summary = String::new();
    if !result.directory_errors.is_empty() {
        issue_summary.push_str(&format!(
            " ({} spec dir(s) failed to scan)",
            result.directory_errors.len()
        ));
    }
    if !lazy_errors.is_empty() {
        issue_summary.push_str(&format!(
            " ({} spec file(s) failed to parse — run `ghost-complete \
             validate-specs` for details)",
            lazy_errors.len()
        ));
    }

    if loaded == 0 {
        // Loud FAIL so a user running `doctor` after a fresh `cargo install`
        // gets an actionable signal instead of silently degraded
        // autocomplete.
        return CheckResult::fail(format!(
            "Completion specs: 0 loaded from {source_count} source(s) \
             [{source_summary}]{issue_summary} — autocomplete will be missing all per-command \
             completions. Run `ghost-complete install` to deploy the \
             bundled spec set."
        ));
    }

    let mut msg = format!(
        "Completion specs: {loaded} loaded from {source_count} source(s) \
         [{source_summary}]"
    );

    if !result.directory_errors.is_empty() || !lazy_errors.is_empty() {
        msg.push_str(&issue_summary);
        return CheckResult::warn(msg);
    }
    CheckResult::ok(msg)
}
/// Check the install mirror's version stamp against the running
/// binary. The auto-refresh path in `main.rs` keeps the mirror in lock
/// step with the embedded corpus on every proxy start — but if that
/// refresh ever fails (read-only home, EACCES, disk full) the stale
/// mirror still wins precedence over the embedded specs at runtime, so
/// the user keeps getting the OLD corpus silently. Surface it loudly
/// here.
fn check_install_mirror_stamp(config: &gc_config::GhostConfig) -> CheckResult {
    // Honour explicit overrides: if the user pointed `[paths] spec_dirs`
    // at a custom location, the default mirror is irrelevant (and we
    // never write to it either — see `auto_refresh_install_mirror_if_stale`).
    if !config.paths.spec_dirs.is_empty() {
        return CheckResult::skip("Spec mirror — skipped (custom [paths] spec_dirs configured)");
    }
    let Some(install_dir) = gc_suggest::mirror::default_install_mirror_dir() else {
        return CheckResult::skip("Spec mirror — HOME unset, cannot resolve install dir");
    };
    let status =
        gc_suggest::mirror::mirror_status(&install_dir, gc_suggest::mirror::CURRENT_VERSION);
    match status {
        gc_suggest::mirror::MirrorStatus::NotInstalled => {
            CheckResult::ok("Spec mirror not installed (using embedded corpus directly)")
        }
        gc_suggest::mirror::MirrorStatus::Fresh => CheckResult::ok(format!(
            "Spec mirror at {} is up to date (v{})",
            install_dir.display(),
            gc_suggest::mirror::CURRENT_VERSION
        )),
        gc_suggest::mirror::MirrorStatus::Unstamped => CheckResult::warn(format!(
            "Spec mirror at {} is from an older version (no version stamp); \
             current binary is v{}. Run `ghost-complete install` to refresh.",
            install_dir.display(),
            gc_suggest::mirror::CURRENT_VERSION
        )),
        gc_suggest::mirror::MirrorStatus::Stale { on_disk } => CheckResult::warn(format!(
            "Spec mirror at {} is from v{on_disk}; current binary is v{}. \
             Run `ghost-complete install` to refresh.",
            install_dir.display(),
            gc_suggest::mirror::CURRENT_VERSION
        )),
    }
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
            .flat_map(|o| o.args.as_ref().into_iter().chain(o.extra_args.iter()))
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
/// `_corrected_in` lifecycle in docs/SPECS.md). This is informational:
/// the affected generators are already disabled by the spec loader, so a
/// fresh local install should not report a health warning for known corpus
/// accounting.
///
/// Re-loads specs with the same runtime resolver `check_specs` uses — cheaper
/// than plumbing the store out of Check 6, and keeps the two checks
/// independent (a broken spec dir still produces a skip here rather than a
/// hard fail).
fn check_corrections(config: &gc_config::GhostConfig) -> CheckResult {
    let result = match load_specs_for_config(config) {
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
            let n = count_corrected_generators_in_spec(spec.as_ref());
            if n == 0 {
                None
            } else {
                Some((name, n))
            }
        })
        .collect();

    if affected_specs.is_empty() {
        return CheckResult::ok("Corrected generators: none");
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

    CheckResult::ok(format!(
        "Corrected generators: {total_generators} generator(s) across {spec_count} spec(s) were \
         previously returning incorrect completions and are now disabled pending \
         proper handling. See CHANGELOG. Affected specs: {preview_str}{tail}"
    ))
}

/// Spec addressability check. Iterates `SpecStore::conflicts()` and
/// reports each alias collision with a kind-specific hint so users can spot
/// rejected aliases and lazy fallback candidates. Pure helper — operates
/// against an in-memory store so it can be unit-tested without touching the
/// resolver.
fn check_alias_conflicts_for_store(store: &gc_suggest::SpecStore) -> CheckResult {
    let conflicts = store.conflicts();
    if conflicts.is_empty() {
        return CheckResult::ok("Spec addressability: no addressability conflicts");
    }

    let dup: Vec<&AliasConflict> = conflicts
        .iter()
        .filter(|c| matches!(c.kind, AliasConflictKind::DuplicateName))
        .collect();
    let cross: Vec<&AliasConflict> = conflicts
        .iter()
        .filter(|c| matches!(c.kind, AliasConflictKind::NameMatchesOtherStem))
        .collect();
    let dir_prec: Vec<&AliasConflict> = conflicts
        .iter()
        .filter(|c| matches!(c.kind, AliasConflictKind::DirectoryPrecedence))
        .collect();

    // Truncate per-class previews so a noisy fallback config (e.g. user
    // override dir overlapping the embedded fallback for every spec)
    // doesn't blow out the doctor output. PREVIEW_LIMIT mirrors the
    // corrected-generators check above.
    const PREVIEW_LIMIT: usize = 5;

    fn render_section(label: &str, hint: &str, items: &[&AliasConflict]) -> String {
        if items.is_empty() {
            return String::new();
        }
        fn disposition_label(disposition: AliasConflictDisposition) -> &'static str {
            match disposition {
                AliasConflictDisposition::Rejected => "rejected",
                AliasConflictDisposition::FallbackCandidate => "fallback",
            }
        }
        let mut s = format!(" {} ({}): {}", label, items.len(), hint);
        let preview = items
            .iter()
            .take(PREVIEW_LIMIT)
            .map(|c| {
                format!(
                    "'{}' ({}={})",
                    c.alias,
                    disposition_label(c.disposition),
                    c.loser.filename_stem
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        if !preview.is_empty() {
            s.push_str(&format!(" — examples: {preview}"));
            if items.len() > PREVIEW_LIMIT {
                s.push_str(&format!(", ...and {} more", items.len() - PREVIEW_LIMIT));
            }
        }
        s
    }

    let actionable_count = dup.len() + cross.len();
    let mut msg = if actionable_count == 0 {
        "Spec addressability: no duplicate/name-stem conflicts.".to_string()
    } else {
        format!("Spec addressability: {actionable_count} duplicate/name-stem conflict(s) detected.")
    };
    msg.push_str(&render_section(
        "DuplicateName",
        "two specs declare the same `name`; rename one in its `name` field, or inspect disposition to see whether the lower-precedence alias is rejected or a lazy fallback",
        &dup,
    ));
    msg.push_str(&render_section(
        "NameMatchesOtherStem",
        "one spec's `name` matches another file's stem (likely corpus inconsistency — file an issue)",
        &cross,
    ));
    msg.push_str(&render_section(
        "DirectoryPrecedence",
        "same filename in multiple configured spec_dirs (earlier dir is preferred; later copy can serve as a lazy-parse fallback)",
        &dir_prec,
    ));

    // DirectoryPrecedence is a deliberate user-override behaviour — keep
    // the severity at Warn only when other kinds are present, otherwise
    // OK with an explanatory note. DuplicateName / NameMatchesOtherStem
    // are corpus problems and surface as Warn.
    if !dup.is_empty() || !cross.is_empty() {
        CheckResult::warn(msg)
    } else {
        CheckResult::ok(msg)
    }
}

/// Entry point that resolves spec dirs and dispatches to
/// [`check_alias_conflicts_for_store`].
fn check_alias_conflicts(config: &gc_config::GhostConfig) -> CheckResult {
    let result = match load_specs_for_config(config) {
        Ok(r) => r,
        Err(_) => {
            return CheckResult::skip(
                "Spec addressability — spec load failed (see Completion specs check)",
            );
        }
    };
    check_alias_conflicts_for_store(&result.store)
}

/// JS runtime kill switch check. Reports a Warn when the runtime is
/// disabled — users running with `js_runtime = false` still see all
/// static completions, but their requires_js generators become inert.
/// Disabling is a valid choice (e.g., to skip QuickJS overhead in
/// resource-constrained environments), so this is informational, not an
/// error.
fn check_js_runtime(config: &gc_config::GhostConfig) -> CheckResult {
    if config.suggest.providers.js_runtime {
        CheckResult::ok("JS runtime: enabled (requires_js generators will run)")
    } else {
        CheckResult::warn(
            "JS runtime: disabled. requires_js generators will not produce dynamic suggestions. Set suggest.providers.js_runtime = true to re-enable.",
        )
    }
}

#[derive(Debug, Default)]
struct AwsCredentialSnapshot {
    selected_profile: Option<String>,
    has_env_access_key: bool,
    has_env_secret_key: bool,
    has_env_session_token: bool,
    env_region: Option<String>,
    file_region_present: bool,
    config_file_exists: bool,
    credentials_file_exists: bool,
    profiles: Vec<String>,
}

fn parse_aws_profile_names(
    config_contents: Option<&str>,
    credentials_contents: Option<&str>,
) -> BTreeSet<String> {
    fn parse_sections(contents: &str, is_config: bool, out: &mut BTreeSet<String>) {
        for line in contents.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
                continue;
            };
            let section = section.trim();
            if section == "default" {
                out.insert("default".to_string());
            } else if is_config {
                if let Some(name) = section.strip_prefix("profile ") {
                    let name = name.trim();
                    if !name.is_empty() {
                        out.insert(name.to_string());
                    }
                }
            } else if !section.contains(' ') {
                out.insert(section.to_string());
            }
        }
    }

    let mut profiles = BTreeSet::new();
    if let Some(contents) = config_contents {
        parse_sections(contents, true, &mut profiles);
    }
    if let Some(contents) = credentials_contents {
        parse_sections(contents, false, &mut profiles);
    }
    profiles
}

fn aws_file_contains_region(contents: Option<&str>) -> bool {
    contents.is_some_and(|contents| {
        contents.lines().any(|line| {
            let line = line.trim();
            !line.starts_with('#')
                && !line.starts_with(';')
                && line
                    .split_once('=')
                    .is_some_and(|(key, value)| key.trim() == "region" && !value.trim().is_empty())
        })
    })
}

fn aws_path_from_env_or_home(env_name: &str, home_suffix: &[&str]) -> Option<PathBuf> {
    match std::env::var_os(env_name) {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => {
            dirs::home_dir().map(|home| home_suffix.iter().fold(home, |path, part| path.join(part)))
        }
    }
}

fn read_optional_file(path: Option<&Path>) -> Option<String> {
    path.filter(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
}

fn aws_env_nonempty(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn aws_env_value(primary: &str, fallback: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var(fallback)
                .ok()
                .filter(|value| !value.is_empty())
        })
}

fn aws_snapshot_from_environment() -> AwsCredentialSnapshot {
    let config_path = aws_path_from_env_or_home("AWS_CONFIG_FILE", &[".aws", "config"]);
    let credentials_path =
        aws_path_from_env_or_home("AWS_SHARED_CREDENTIALS_FILE", &[".aws", "credentials"]);
    let config_file_exists = config_path.as_deref().is_some_and(Path::exists);
    let credentials_file_exists = credentials_path.as_deref().is_some_and(Path::exists);
    let config_contents = read_optional_file(config_path.as_deref());
    let credentials_contents = read_optional_file(credentials_path.as_deref());
    let profiles =
        parse_aws_profile_names(config_contents.as_deref(), credentials_contents.as_deref())
            .into_iter()
            .collect();

    AwsCredentialSnapshot {
        selected_profile: aws_env_value("AWS_PROFILE", "AWS_DEFAULT_PROFILE"),
        has_env_access_key: aws_env_nonempty("AWS_ACCESS_KEY_ID"),
        has_env_secret_key: aws_env_nonempty("AWS_SECRET_ACCESS_KEY"),
        has_env_session_token: aws_env_nonempty("AWS_SESSION_TOKEN"),
        env_region: aws_env_value("AWS_REGION", "AWS_DEFAULT_REGION"),
        file_region_present: aws_file_contains_region(config_contents.as_deref())
            || aws_file_contains_region(credentials_contents.as_deref()),
        config_file_exists,
        credentials_file_exists,
        profiles,
    }
}

fn profile_summary(profiles: &[String]) -> String {
    const LIMIT: usize = 8;
    if profiles.is_empty() {
        return "profiles: none".to_string();
    }
    let mut listed = profiles.iter().take(LIMIT).cloned().collect::<Vec<_>>();
    if profiles.len() > LIMIT {
        listed.push(format!("...and {} more", profiles.len() - LIMIT));
    }
    format!("profiles: {}", listed.join(", "))
}

fn check_aws_credentials_from_snapshot(
    config: &gc_config::GhostConfig,
    snapshot: AwsCredentialSnapshot,
) -> CheckResult {
    let provider = if config.experimental.aws_sdk_provider {
        "enabled"
    } else {
        "disabled"
    };
    let fallback = if config.experimental.aws_sdk_fallback_to_cli {
        "CLI fallback enabled"
    } else {
        "CLI fallback disabled"
    };
    let env_creds_complete = snapshot.has_env_access_key && snapshot.has_env_secret_key;
    let env_creds = if env_creds_complete {
        if snapshot.has_env_session_token {
            "env credentials detected with session token"
        } else {
            "env credentials detected"
        }
    } else if snapshot.has_env_access_key || snapshot.has_env_secret_key {
        "partial env credentials"
    } else {
        "no env credentials"
    };
    let files = match (
        snapshot.config_file_exists,
        snapshot.credentials_file_exists,
    ) {
        (true, true) => "AWS config and credentials files present",
        (true, false) => "AWS config file present, credentials file missing",
        (false, true) => "AWS credentials file present, config file missing",
        (false, false) => "no AWS profile files",
    };
    let region = snapshot
        .env_region
        .as_deref()
        .map(|region| format!("region from env: {region}"))
        .unwrap_or_else(|| {
            if snapshot.file_region_present {
                "region found in AWS files".to_string()
            } else {
                "region not visible in env/files".to_string()
            }
        });
    let selected_profile_missing = snapshot
        .selected_profile
        .as_ref()
        .is_some_and(|selected| !snapshot.profiles.iter().any(|profile| profile == selected));

    let mut message = format!(
        "AWS credentials: AWS SDK provider: {provider}; {fallback}; {env_creds}; {files}; {}; {region}",
        profile_summary(&snapshot.profiles),
    );

    if let Some(profile) = snapshot.selected_profile.as_deref() {
        message.push_str(&format!("; selected profile: '{profile}'"));
    }

    if selected_profile_missing {
        let profile = snapshot.selected_profile.as_deref().unwrap_or_default();
        message.push_str(&format!("; selected profile '{profile}' not found"));
    }

    if !config.experimental.aws_sdk_provider {
        message.push_str("; no outbound AWS SDK calls unless aws_sdk_provider is enabled");
        return CheckResult::ok(message);
    }

    if selected_profile_missing {
        return CheckResult::warn(message);
    }
    if !env_creds_complete && snapshot.profiles.is_empty() {
        return CheckResult::warn(message);
    }
    if snapshot.env_region.is_none() && !snapshot.file_region_present {
        return CheckResult::warn(message);
    }
    CheckResult::ok(message)
}

fn check_aws_credentials(config: &gc_config::GhostConfig) -> CheckResult {
    check_aws_credentials_from_snapshot(config, aws_snapshot_from_environment())
}

/// Warn when any `keep_warm` entry does not match a registered spec alias.
/// Returns OK with an explanatory message when eviction is disabled.
fn check_keep_warm_unmatched(
    store: &gc_suggest::SpecStore,
    cfg: &gc_config::SpecCacheConfig,
) -> CheckResult {
    if !cfg.enabled() {
        return CheckResult::ok("spec_cache.keep_warm: eviction disabled");
    }

    let registered: HashSet<&str> = store
        .entries()
        .iter()
        .flat_map(|e| e.aliases.iter().map(String::as_str))
        .collect();
    let unmatched: Vec<&str> = cfg
        .keep_warm
        .iter()
        .filter(|name| !registered.contains(name.as_str()))
        .map(String::as_str)
        .collect();

    if unmatched.is_empty() {
        return CheckResult::ok("spec_cache.keep_warm: all entries match registered aliases");
    }

    let suggestions = unmatched
        .iter()
        .map(|name| {
            nearest_alias(name, &registered)
                .map(|near| format!("'{name}' -> did you mean '{near}'?"))
                .unwrap_or_else(|| format!("'{name}' (no near match)"))
        })
        .collect::<Vec<_>>()
        .join("; ");

    CheckResult::warn(format!(
        "spec_cache.keep_warm has {} unmatched alias(es): {suggestions}",
        unmatched.len()
    ))
}

fn nearest_alias(target: &str, registered: &HashSet<&str>) -> Option<String> {
    registered
        .iter()
        .map(|alias| (*alias, levenshtein(target, alias)))
        .filter(|(_, distance)| *distance <= 2)
        .min_by(
            |(left_alias, left_distance), (right_alias, right_distance)| {
                left_distance
                    .cmp(right_distance)
                    .then_with(|| left_alias.cmp(right_alias))
            },
        )
        .map(|(alias, _)| alias.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Warn when estimated resident heap exceeds 90% of `max_resident_mb`.
fn check_resident_near_cap(
    store: &gc_suggest::SpecStore,
    cfg: &gc_config::SpecCacheConfig,
) -> CheckResult {
    if !cfg.enabled() {
        return CheckResult::ok("spec_cache resident cap: eviction disabled");
    }
    let Some(cap) = cfg.max_resident_bytes() else {
        return CheckResult::ok("spec_cache resident cap: no cap configured");
    };
    let resident = store.estimated_resident_bytes();
    let threshold = cap.saturating_mul(9) / 10;
    if resident > threshold {
        CheckResult::warn(format!(
            "spec_cache resident ~{} MB is >90% of cap ({} MB); consider raising \
             max_resident_mb or shortening idle_ttl_secs",
            resident / (1024 * 1024),
            cfg.max_resident_mb,
        ))
    } else {
        CheckResult::ok("spec_cache resident cap: below warning threshold")
    }
}

fn check_spec_cache_for_store(
    store: &gc_suggest::SpecStore,
    cfg: &gc_config::SpecCacheConfig,
) -> Vec<CheckResult> {
    let mut results = vec![check_keep_warm_unmatched(store, cfg)];
    if cfg.enabled() && cfg.max_resident_bytes().is_some() {
        // Doctor is an explicit inspection command. Force-load resolved specs
        // so resident-cap pressure reflects the parsed heap a warm runtime
        // would actually hold.
        let _ = store.iter().count();
    }
    results.push(check_resident_near_cap(store, cfg));
    results
}

fn check_spec_cache(config: &gc_config::GhostConfig) -> Vec<CheckResult> {
    match load_specs_for_config(config) {
        Ok(result) => check_spec_cache_for_store(&result.store, &config.suggest.spec_cache),
        Err(_) => vec![
            CheckResult::skip(
                "spec_cache.keep_warm — spec load failed (see Completion specs check)",
            ),
            CheckResult::skip(
                "spec_cache.resident_cap — spec load failed (see Completion specs check)",
            ),
        ],
    }
}

#[derive(Debug, Default)]
struct RuntimeMetadataCounts {
    malformed: usize,
    unsupported_unproven: usize,
}

/// Walk a parsed spec and yield counts for `requires_js: true` generators
/// that are either malformed or intentionally unsupported by the engine:
///   * missing `js_runtime` metadata entirely, OR
///   * `js_runtime.source` is empty/whitespace, OR
///   * `js_runtime.kind` is `post_process` AND neither `script` nor
///     `script_template` has a non-empty argv (the engine has no shell
///     stdout to feed into the post-processor),
///   * `js_runtime.kind` is `script_function` / `custom` AND
///     `self_contained != true`.
///
/// The first three are malformed corpus entries and should fail doctor.
/// The last class is tracked as unsupported coverage: the engine skips it
/// because the converter has not proven the JS source self-contained, but
/// those entries are expected in the current corpus baseline and should
/// remain an OK health result.
///
/// Mirrors the engine's `is_supported_script_generator` predicate
/// (gc-suggest::engine), but preserves the fatal-vs-unsupported distinction
/// for operator-facing health checks.
fn count_runtime_metadata_issues_in_spec(spec: &CompletionSpec) -> RuntimeMetadataCounts {
    enum Issue {
        Malformed,
        UnsupportedUnproven,
    }

    fn has_non_empty_script_or_template(g: &GeneratorSpec) -> bool {
        g.script.as_ref().is_some_and(|script| !script.is_empty())
            || g.script_template
                .as_ref()
                .is_some_and(|template| !template.is_empty())
    }

    fn issue(g: &GeneratorSpec) -> Option<Issue> {
        if !g.requires_js {
            return None;
        }
        match g.js_runtime.as_ref() {
            None => Some(Issue::Malformed),
            Some(rt) => {
                if rt.source.trim().is_empty() {
                    return Some(Issue::Malformed);
                }
                match rt.kind {
                    JsRuntimeKind::PostProcess => {
                        if !has_non_empty_script_or_template(g) {
                            Some(Issue::Malformed)
                        } else {
                            None
                        }
                    }
                    JsRuntimeKind::ScriptFunction | JsRuntimeKind::Custom => {
                        if rt.self_contained {
                            None
                        } else {
                            Some(Issue::UnsupportedUnproven)
                        }
                    }
                    JsRuntimeKind::TokenOnly => None,
                }
            }
        }
    }

    fn add_issue(counts: &mut RuntimeMetadataCounts, issue: Issue) {
        match issue {
            Issue::Malformed => counts.malformed += 1,
            Issue::UnsupportedUnproven => counts.unsupported_unproven += 1,
        }
    }

    fn count_in_args(args: &[ArgSpec], counts: &mut RuntimeMetadataCounts) {
        for issue in args
            .iter()
            .flat_map(|a| a.generators.iter())
            .filter_map(issue)
        {
            add_issue(counts, issue);
        }
    }

    fn count_in_options(options: &[OptionSpec], counts: &mut RuntimeMetadataCounts) {
        for issue in options
            .iter()
            .flat_map(|o| o.args.as_ref().into_iter().chain(o.extra_args.iter()))
            .flat_map(|a| a.generators.iter())
            .filter_map(issue)
        {
            add_issue(counts, issue);
        }
    }

    let mut counts = RuntimeMetadataCounts::default();
    count_in_args(&spec.args, &mut counts);
    count_in_options(&spec.options, &mut counts);
    let mut stack: Vec<&SubcommandSpec> = spec.subcommands.iter().collect();
    while let Some(sub) = stack.pop() {
        count_in_args(&sub.args, &mut counts);
        count_in_options(&sub.options, &mut counts);
        stack.extend(sub.subcommands.iter());
    }
    counts
}

/// Backwards-compatible helper for tests that only care whether the spec has
/// fatal malformed runtime metadata.
fn count_missing_js_runtime_in_spec(spec: &CompletionSpec) -> usize {
    count_runtime_metadata_issues_in_spec(spec).malformed
}

/// Count `requires_js: true` generators the engine will skip because their
/// `script_function` / `custom` source has not been proven self-contained.
fn count_unproven_js_runtime_in_spec(spec: &CompletionSpec) -> usize {
    count_runtime_metadata_issues_in_spec(spec).unsupported_unproven
}

/// Embedded specs runtime-source check. Walks every entry in the
/// SpecStore (including embedded fallback). Malformed JS runtime metadata
/// warns (engine skips the generator silently, same runtime impact as
/// unproven `script_function` / `custom`, but the converter regen path
/// should still surface it to a maintainer). Unproven generators are
/// expected unsupported coverage and remain an OK result.
fn check_embedded_runtime_metadata_for_store(store: &gc_suggest::SpecStore) -> CheckResult {
    let mut affected: Vec<(&str, usize)> = store
        .iter()
        .filter_map(|(name, spec)| {
            let n = count_missing_js_runtime_in_spec(spec.as_ref());
            if n == 0 {
                None
            } else {
                Some((name, n))
            }
        })
        .collect();
    let mut unproven: Vec<(&str, usize)> = store
        .iter()
        .filter_map(|(name, spec)| {
            let n = count_unproven_js_runtime_in_spec(spec.as_ref());
            if n == 0 {
                None
            } else {
                Some((name, n))
            }
        })
        .collect();

    if affected.is_empty() && unproven.is_empty() {
        return CheckResult::ok(
            "Embedded specs: every requires_js generator has dispatchable js_runtime metadata",
        );
    }

    affected.sort_by_key(|(name, _)| *name);
    unproven.sort_by_key(|(name, _)| *name);
    const PREVIEW_LIMIT: usize = 5;

    if affected.is_empty() {
        let total: usize = unproven.iter().map(|(_, n)| *n).sum();
        let spec_count = unproven.len();
        let preview: Vec<&str> = unproven
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
        return CheckResult::ok(format!(
            "Embedded specs: {total} requires_js generator(s) across {spec_count} spec(s) are \
             unsupported and will be skipped (`script_function`/`custom` without \
             `self_contained:true`). This is tracked by `ghost-complete status`. \
             Affected: {preview_str}{tail}"
        ));
    }

    let total: usize = affected.iter().map(|(_, n)| *n).sum();
    let spec_count = affected.len();
    let preview: Vec<&str> = affected
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
        "Embedded specs: {total} requires_js generator(s) across {spec_count} spec(s) cannot be \
         dispatched (missing js_runtime metadata, empty `js_runtime.source`, `post_process` kind \
         without a non-empty `script`/`script_template`). Engine skips them silently; regenerate \
         the corpus or remove the stale `requires_js: true` to clear. Affected: \
         {preview_str}{tail}"
    ))
}

/// Entry point that resolves spec dirs and dispatches to
/// [`check_embedded_runtime_metadata_for_store`].
fn check_embedded_runtime_metadata(config: &gc_config::GhostConfig) -> CheckResult {
    let result = match load_specs_for_config(config) {
        Ok(r) => r,
        Err(_) => {
            return CheckResult::skip(
                "Embedded specs — spec load failed (see Completion specs check)",
            );
        }
    };
    check_embedded_runtime_metadata_for_store(&result.store)
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

    // Spec mirror stamp: surfaces stale `~/.config/ghost-complete/specs/`
    // when the auto-refresh on proxy startup could not run (read-only
    // home, EACCES, etc.). Without this check, an upgrade where the
    // mirror auto-refresh silently fails leaves the user serving stale
    // specs at every keystroke with no visible signal.
    match &config {
        Some(cfg) => results.push(check_install_mirror_stamp(cfg)),
        None => results.push(CheckResult::skip(
            "Spec mirror — config invalid, cannot resolve install dir",
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

    // Spec addressability surfaces AliasConflicts so users can spot a
    // `name` that lost to another file's stem. The JS runtime check warns
    // when the kill switch is off so a user who forgot they disabled it
    // sees why their dynamic completions are inert. The embedded specs
    // check asserts every requires_js generator in the loaded corpus
    // carries js_runtime metadata.
    match &config {
        Some(cfg) => {
            results.push(check_alias_conflicts(cfg));
            results.push(check_js_runtime(cfg));
            results.push(check_aws_credentials(cfg));
            results.push(check_embedded_runtime_metadata(cfg));
            results.extend(check_spec_cache(cfg));
        }
        None => {
            results.push(CheckResult::skip(
                "Spec addressability — config invalid, cannot resolve spec dirs",
            ));
            results.push(CheckResult::skip("JS runtime — config invalid"));
            results.push(CheckResult::skip("AWS credentials — config invalid"));
            results.push(CheckResult::skip(
                "Embedded specs — config invalid, cannot resolve spec dirs",
            ));
            results.push(CheckResult::skip("spec_cache.keep_warm — config invalid"));
            results.push(CheckResult::skip(
                "spec_cache.resident_cap — config invalid",
            ));
        }
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

    #[test]
    fn aws_profile_parser_reads_config_and_credentials_profiles() {
        let profiles = parse_aws_profile_names(
            Some(
                r#"
[default]
region = us-east-1
[profile dev]
region = eu-west-1
[sso-session corp]
sso_start_url = https://example.awsapps.com/start
"#,
            ),
            Some(
                r#"
[prod]
aws_access_key_id = AKIA...
[default]
aws_access_key_id = AKIA...
"#,
            ),
        );

        assert_eq!(
            profiles.into_iter().collect::<Vec<_>>(),
            vec!["default", "dev", "prod"]
        );
    }

    #[test]
    fn aws_credentials_check_reports_disabled_provider_without_warning() {
        let mut config = gc_config::GhostConfig::default();
        config.experimental.aws_sdk_provider = false;
        config.experimental.aws_sdk_fallback_to_cli = true;
        let snapshot = AwsCredentialSnapshot {
            profiles: vec!["default".to_string()],
            config_file_exists: true,
            credentials_file_exists: true,
            ..Default::default()
        };

        let result = check_aws_credentials_from_snapshot(&config, snapshot);

        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("AWS SDK provider: disabled"));
        assert!(result.message.contains("CLI fallback enabled"));
        assert!(result.message.contains("profiles: default"));
    }

    #[test]
    fn aws_credentials_check_warns_when_enabled_without_credential_signal() {
        let mut config = gc_config::GhostConfig::default();
        config.experimental.aws_sdk_provider = true;
        let snapshot = AwsCredentialSnapshot::default();

        let result = check_aws_credentials_from_snapshot(&config, snapshot);

        assert!(matches!(result.severity, Severity::Warn));
        assert!(result.message.contains("AWS credentials"));
        assert!(result.message.contains("no env credentials"));
        assert!(result.message.contains("no AWS profile files"));
    }

    #[test]
    fn aws_credentials_check_warns_when_selected_profile_is_missing() {
        let mut config = gc_config::GhostConfig::default();
        config.experimental.aws_sdk_provider = true;
        let snapshot = AwsCredentialSnapshot {
            selected_profile: Some("staging".to_string()),
            profiles: vec!["default".to_string(), "dev".to_string()],
            config_file_exists: true,
            credentials_file_exists: true,
            ..Default::default()
        };

        let result = check_aws_credentials_from_snapshot(&config, snapshot);

        assert!(matches!(result.severity, Severity::Warn));
        assert!(result
            .message
            .contains("selected profile 'staging' not found"));
    }

    #[test]
    fn doctor_warns_when_keep_warm_entry_unmatched() {
        let store = gc_suggest::SpecStore::load_with_embedded(&[])
            .unwrap()
            .store;
        let cfg = gc_config::SpecCacheConfig {
            idle_ttl_secs: 300,
            keep_warm: vec!["giit".to_string()],
            ..Default::default()
        };

        let result = check_keep_warm_unmatched(&store, &cfg);

        assert!(matches!(result.severity, Severity::Warn));
        assert!(result.message.contains("spec_cache.keep_warm"));
        assert!(result.message.contains("giit"));
        assert!(result.message.contains("did you mean 'git'"));
    }

    #[test]
    fn doctor_keep_warm_unmatched_skips_disabled_eviction() {
        let store = gc_suggest::SpecStore::load_with_embedded(&[])
            .unwrap()
            .store;
        let cfg = gc_config::SpecCacheConfig {
            idle_ttl_secs: 0,
            keep_warm: vec!["nonexistent-spec".to_string()],
            ..Default::default()
        };

        let result = check_keep_warm_unmatched(&store, &cfg);

        assert!(matches!(result.severity, Severity::Ok));
    }

    #[test]
    fn doctor_keep_warm_unmatched_ok_when_all_match() {
        let store = gc_suggest::SpecStore::load_with_embedded(&[])
            .unwrap()
            .store;
        let cfg = gc_config::SpecCacheConfig {
            idle_ttl_secs: 300,
            keep_warm: vec!["git".to_string()],
            ..Default::default()
        };

        let result = check_keep_warm_unmatched(&store, &cfg);

        assert!(matches!(result.severity, Severity::Ok));
        assert!(result
            .message
            .contains("all entries match registered aliases"));
    }

    #[test]
    fn doctor_warns_at_90pct_resident_cap() {
        let dir = tempfile::TempDir::new().unwrap();
        let large_suggestion = "x".repeat(1_000_000);
        let body = format!(
            r#"{{
                "name": "big",
                "args": [{{
                    "name": "target",
                    "suggestions": ["{large_suggestion}"]
                }}]
            }}"#
        );
        std::fs::write(dir.path().join("big.json"), body).unwrap();
        let store = gc_suggest::SpecStore::load_from_dir(dir.path())
            .unwrap()
            .store;
        let _ = store.get("big");
        let cfg = gc_config::SpecCacheConfig {
            idle_ttl_secs: 300,
            max_resident_mb: 1,
            ..Default::default()
        };

        let result = check_resident_near_cap(&store, &cfg);

        assert!(matches!(result.severity, Severity::Warn));
        assert!(result.message.contains("spec_cache resident"));
        assert!(result.message.contains(">90% of cap"));
    }

    /// OK branch: eviction is enabled and a cap is configured, but the
    /// resident heap sits well below 90% of it. Pins the comparison so a
    /// regression that flips `>` to `>=` or drops the threshold math would
    /// fail loudly.
    #[test]
    fn doctor_resident_cap_ok_when_below_threshold() {
        let store = gc_suggest::SpecStore::load_with_embedded(&[])
            .unwrap()
            .store;
        // Force-load every entry so resident_bytes reflects the full
        // parsed corpus (the same heap an uncapped daemon would hold).
        let _ = store.iter().count();
        let cfg = gc_config::SpecCacheConfig {
            idle_ttl_secs: 300,
            max_resident_mb: 1024,
            ..Default::default()
        };

        let result = check_resident_near_cap(&store, &cfg);

        assert!(
            matches!(result.severity, Severity::Ok),
            "resident heap below 90% of cap must be OK: {}",
            result.message
        );
        assert!(
            result.message.contains("below warning threshold"),
            "OK message should name the threshold check: {}",
            result.message
        );
    }

    /// OK branch: eviction is enabled but no resident cap is configured
    /// (`max_resident_mb = 0`). The check must short-circuit with an OK
    /// result naming the missing cap, never compute a threshold against
    /// an unset value.
    #[test]
    fn doctor_resident_cap_ok_when_no_cap_with_eviction_enabled() {
        let store = gc_suggest::SpecStore::load_with_embedded(&[])
            .unwrap()
            .store;
        let cfg = gc_config::SpecCacheConfig {
            idle_ttl_secs: 300,
            max_resident_mb: 0,
            ..Default::default()
        };

        let result = check_resident_near_cap(&store, &cfg);

        assert!(
            matches!(result.severity, Severity::Ok),
            "no-cap config must produce an OK result: {}",
            result.message
        );
        assert!(
            result.message.contains("no cap configured"),
            "OK message should explain the unset cap: {}",
            result.message
        );
    }

    #[test]
    fn doctor_spec_cache_check_force_loads_for_resident_cap() {
        let dir = tempfile::TempDir::new().unwrap();
        let large_suggestion = "x".repeat(1_000_000);
        let body = format!(
            r#"{{
                "name": "big",
                "args": [{{
                    "name": "target",
                    "suggestions": ["{large_suggestion}"]
                }}]
            }}"#
        );
        std::fs::write(dir.path().join("big.json"), body).unwrap();
        let store = gc_suggest::SpecStore::load_from_dir(dir.path())
            .unwrap()
            .store;
        assert_eq!(
            store.parsed_count(),
            0,
            "fixture should start lazy so this test covers doctor's force-load path"
        );
        let cfg = gc_config::SpecCacheConfig {
            idle_ttl_secs: 300,
            max_resident_mb: 1,
            ..Default::default()
        };

        let results = check_spec_cache_for_store(&store, &cfg);

        assert!(
            matches!(results[1].severity, Severity::Warn),
            "resident cap check should warn after doctor force-loads parsed specs: {}",
            results[1].message
        );
        assert_eq!(store.parsed_count(), 1);
    }

    /// Pin the user-facing spec health check to the embedded fallback path.
    ///
    /// `check_specs` calls `resolve_spec_dirs_with_provenance` and preserves
    /// the PTY proxy's embedded fallback policy, and must never report OK
    /// with zero specs loaded.
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

    #[test]
    fn check_specs_reports_lazy_parse_failures() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("broken.json"), "{not valid json").unwrap();

        let result = check_specs_for_resolution(&[dir.path().to_path_buf()], false);

        assert!(
            matches!(result.severity, Severity::Fail),
            "all-broken spec directory must fail: {}",
            result.message
        );
        assert!(
            result.message.contains("failed to parse") || result.message.contains("failed"),
            "message should surface lazy parse failure: {}",
            result.message
        );
    }

    #[test]
    fn check_specs_warns_for_broken_override_with_embedded_fallback() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("git.json"), "{not valid json").unwrap();

        let result = check_specs_for_resolution(&[dir.path().to_path_buf()], true);

        assert!(
            matches!(result.severity, Severity::Warn),
            "broken filesystem override with embedded fallback must warn: {}",
            result.message
        );
        assert!(
            result.message.starts_with("Completion specs:"),
            "unexpected message shape: {}",
            result.message
        );
        assert!(
            !result.message.starts_with("Completion specs: 0 loaded"),
            "embedded fallback should keep usable specs loaded: {}",
            result.message
        );
        assert!(
            result.message.contains("1 spec file(s) failed to parse"),
            "message should report the parse-failure count: {}",
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
            result.directory_errors.is_empty(),
            "fixture directory load errors: {:?}",
            result.directory_errors
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
            result.message.contains("Corrected generators: none"),
            "unexpected message: {}",
            result.message
        );
    }

    #[test]
    fn check_corrections_for_store_reports_ok_when_present() {
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
            matches!(result.severity, Severity::Ok),
            "expected OK, got message: {}",
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
        assert!(matches!(result.severity, Severity::Ok));
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

    // -------------------------------------------------------------------------
    // alias conflicts / JS runtime / embedded specs
    // -------------------------------------------------------------------------

    #[test]
    fn doctor_lists_alias_conflicts_with_kind_specific_hints() {
        // Two specs declare the same `name` — duplicate_name conflict.
        let (store, _dir) = store_from_json_fixtures(&[
            ("a.json", r#"{"name":"shared"}"#),
            ("b.json", r#"{"name":"shared"}"#),
        ]);
        let result = check_alias_conflicts_for_store(&store);
        assert!(matches!(result.severity, Severity::Warn));
        assert!(
            result.message.contains("conflict(s) detected"),
            "message must lead with conflict count: {}",
            result.message
        );
        assert!(
            result.message.contains("DuplicateName"),
            "message must label the conflict kind: {}",
            result.message
        );
        assert!(
            result.message.contains("rename one"),
            "message must include the kind-specific hint: {}",
            result.message
        );
        assert!(
            result.message.contains("'shared'"),
            "message must name the contended alias: {}",
            result.message
        );
    }

    #[test]
    fn doctor_alias_conflicts_ok_when_none() {
        let (store, _dir) = store_from_json_fixtures(&[("a.json", r#"{"name":"a"}"#)]);
        let result = check_alias_conflicts_for_store(&store);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("no addressability conflicts"));
    }

    #[test]
    fn doctor_directory_precedence_is_ok_without_conflict_wording() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("primary");
        let fallback = tmp.path().join("fallback");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(primary.join("git.json"), r#"{"name":"git"}"#).unwrap();
        std::fs::write(fallback.join("git.json"), r#"{"name":"git"}"#).unwrap();

        let result =
            gc_suggest::SpecStore::load_from_dirs(&[primary, fallback]).expect("load fixtures");
        let check = check_alias_conflicts_for_store(&result.store);
        assert!(
            matches!(check.severity, Severity::Ok),
            "directory precedence should be OK: {}",
            check.message
        );
        assert!(
            check.message.contains("no duplicate/name-stem conflicts"),
            "directory precedence should not be framed as an actionable conflict: {}",
            check.message
        );
    }

    #[test]
    fn doctor_warns_when_js_runtime_disabled() {
        let mut config = gc_config::GhostConfig::default();
        config.suggest.providers.js_runtime = false;
        let result = check_js_runtime(&config);
        assert!(matches!(result.severity, Severity::Warn));
        assert!(
            result.message.contains("disabled"),
            "expected `disabled` in message: {}",
            result.message
        );
        assert!(
            result
                .message
                .contains("suggest.providers.js_runtime = true"),
            "expected pointer at config key: {}",
            result.message
        );
    }

    #[test]
    fn doctor_passes_when_js_runtime_enabled() {
        let mut config = gc_config::GhostConfig::default();
        config.suggest.providers.js_runtime = true;
        let result = check_js_runtime(&config);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(result.message.contains("enabled"));
    }

    #[test]
    fn doctor_passes_when_runtime_metadata_complete() {
        // Spec with a requires_js generator that carries dispatchable
        // js_runtime metadata (post_process + script + non-empty source).
        // Should be OK.
        let (store, _dir) = store_from_json_fixtures(&[(
            "ok.json",
            r#"{
                "name": "ok",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "script": ["cmd"],
                        "requires_js": true,
                        "js_runtime": {"kind":"post_process","source":"()=>[]"}
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(matches!(result.severity, Severity::Ok));
        assert!(
            result
                .message
                .contains("every requires_js generator has dispatchable js_runtime metadata"),
            "got: {}",
            result.message
        );
    }

    #[test]
    fn doctor_warns_when_runtime_metadata_missing() {
        // Spec with requires_js=true but no js_runtime — the converter
        // forgot to populate. Engine skips at runtime, but the doctor
        // surfaces a WARN so a regen mistake stays visible without
        // exit-1ing every clean install (the converted corpus ships
        // ~295 such entries that are stale-but-harmless).
        let (store, _dir) = store_from_json_fixtures(&[(
            "broken.json",
            r#"{
                "name": "broken",
                "args": [{
                    "name": "x",
                    "generators": [{"requires_js": true}]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(matches!(result.severity, Severity::Warn));
        assert!(
            result.message.contains("missing js_runtime metadata"),
            "got: {}",
            result.message
        );
        assert!(
            result.message.contains("broken"),
            "must name affected spec: {}",
            result.message
        );
    }

    #[test]
    fn doctor_warns_when_second_option_arg_runtime_metadata_missing() {
        // Fig permits option args as an array. The doctor check must inspect
        // more than the first element so converter regressions in later option
        // args still surface (as a WARN — see
        // `doctor_warns_when_runtime_metadata_missing` for the severity
        // rationale).
        let (store, _dir) = store_from_json_fixtures(&[(
            "option-args.json",
            r#"{
                "name": "option-args",
                "options": [{
                    "name": ["--format"],
                    "args": [
                        {
                            "name": "first",
                            "generators": [{
                                "requires_js": true,
                                "js_runtime": {"kind":"custom","source":"()=>[]"}
                            }]
                        },
                        {
                            "name": "second",
                            "generators": [{"requires_js": true}]
                        }
                    ]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Warn),
            "expected Warn, got message: {}",
            result.message
        );
        assert!(
            result.message.contains("missing js_runtime metadata"),
            "got: {}",
            result.message
        );
        assert!(
            result.message.contains("option-args"),
            "must name affected spec: {}",
            result.message
        );
    }

    #[test]
    fn doctor_warns_when_runtime_source_empty() {
        // js_runtime present but source is whitespace — the converter
        // dropped the body. Same severity as missing entirely.
        let (store, _dir) = store_from_json_fixtures(&[(
            "empty-source.json",
            r#"{
                "name": "empty",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "requires_js": true,
                        "js_runtime": {"kind":"custom","source":"   "}
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(matches!(result.severity, Severity::Warn));
    }

    /// A `script_function` / `custom` generator whose `self_contained` is
    /// missing or `false` cannot be dispatched by the engine, but this is
    /// expected unsupported coverage rather than malformed metadata. Doctor
    /// must stay OK so a fresh local install has no warning while still
    /// exposing the skipped-generator count.
    #[test]
    fn doctor_is_ok_when_script_function_lacks_self_contained() {
        let (store, _dir) = store_from_json_fixtures(&[(
            "unproven.json",
            r#"{
                "name": "unproven",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "script_function",
                            "source": "() => ['a']"
                        }
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Ok),
            "script_function without self_contained must be OK, got: {}",
            result.message
        );
        assert!(
            result.message.contains("unproven"),
            "must name affected spec: {}",
            result.message
        );
        assert!(
            result.message.contains("unsupported") && result.message.contains("self_contained"),
            "must explain unsupported self_contained class: {}",
            result.message
        );
    }

    #[test]
    fn doctor_is_ok_when_custom_lacks_self_contained() {
        let (store, _dir) = store_from_json_fixtures(&[(
            "custom-unproven.json",
            r#"{
                "name": "custom-unproven",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "custom",
                            "source": "async () => [{name: 'a'}]",
                            "self_contained": false
                        }
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Ok),
            "custom with self_contained:false must be OK, got: {}",
            result.message
        );
    }

    /// `token_only` is the sandboxed runtime — its source receives only the
    /// captured user tokens and never the host's cwd/env, so the
    /// `self_contained` gate that motivates the `script_function`/`custom`
    /// classification does not apply. A future refactor that re-classifies
    /// `TokenOnly` as `Issue::UnsupportedUnproven` (e.g. by collapsing the
    /// match arm under `_` or moving the self_contained gate up) would flip
    /// the doctor severity for every promoted spec and surface a spurious
    /// "unsupported (unproven self_contained)" message. This test pins the
    /// current `JsRuntimeKind::TokenOnly => None` arm so that regression is
    /// caught loudly.
    #[test]
    fn doctor_is_ok_when_token_only_lacks_self_contained() {
        let (store, _dir) = store_from_json_fixtures(&[(
            "token-only.json",
            r#"{
                "name": "token-only",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "token_only",
                            "source": "tokens.map(name => ({name}))",
                            "self_contained": false
                        }
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Ok),
            "token_only with self_contained:false must be OK, got: {}",
            result.message
        );
        assert!(
            !result.message.contains("self_contained"),
            "token_only must not surface an unsupported-self_contained warning: {}",
            result.message
        );
        assert!(
            !result.message.contains("unsupported"),
            "token_only must not surface an unsupported-class warning: {}",
            result.message
        );
    }

    /// Companion positive control: a `script_function` proven
    /// `self_contained: true` with a non-empty source IS dispatchable
    /// and must not Fail. Without this we cannot triangulate a
    /// regression in the new self_contained branch.
    #[test]
    fn doctor_passes_when_script_function_is_self_contained() {
        let (store, _dir) = store_from_json_fixtures(&[(
            "proven.json",
            r#"{
                "name": "proven",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "script_function",
                            "source": "() => ['ls', '-la']",
                            "self_contained": true
                        }
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Ok),
            "script_function with self_contained:true must be OK, got: {}",
            result.message
        );
    }

    /// `post_process` is exempt from the `self_contained` requirement —
    /// the JS body only handles shell stdout, so the bundler-helper
    /// closure surface that motivates the gate doesn't apply.
    #[test]
    fn doctor_passes_when_post_process_lacks_self_contained() {
        let (store, _dir) = store_from_json_fixtures(&[(
            "pp.json",
            r#"{
                "name": "pp",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "script": ["cmd"],
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "post_process",
                            "source": "out => out.split('\n')"
                        }
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Ok),
            "post_process without self_contained must remain OK, got: {}",
            result.message
        );
    }

    /// Regression guard for sf-iter3-2: `post_process` requires an
    /// accompanying `script` or `script_template` (the engine has no
    /// shell stdout to feed the post-processor). The engine's
    /// `is_supported_script_generator` predicate enforces this — the
    /// doctor must too, otherwise an operator gets a green doctor
    /// result while the engine silently filters the generator at
    /// dispatch.
    #[test]
    fn doctor_warns_when_post_process_lacks_script() {
        let (store, _dir) = store_from_json_fixtures(&[(
            "pp_no_script.json",
            r#"{
                "name": "pp_no_script",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "post_process",
                            "source": "out => out.split('\n')"
                        }
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Warn),
            "post_process without script/script_template must Warn, got: {}",
            result.message
        );
        assert!(
            result.message.contains("pp_no_script"),
            "must name affected spec: {}",
            result.message
        );
    }

    #[test]
    fn doctor_warns_when_post_process_script_argv_is_empty() {
        let (store, _dir) = store_from_json_fixtures(&[
            (
                "pp_empty_script.json",
                r#"{
                    "name": "pp_empty_script",
                    "args": [{
                        "name": "x",
                        "generators": [{
                            "script": [],
                            "requires_js": true,
                            "js_runtime": {
                                "kind": "post_process",
                                "source": "out => out.split('\n')"
                            }
                        }]
                    }]
                }"#,
            ),
            (
                "pp_empty_template.json",
                r#"{
                    "name": "pp_empty_template",
                    "args": [{
                        "name": "x",
                        "generators": [{
                            "script_template": [],
                            "requires_js": true,
                            "js_runtime": {
                                "kind": "post_process",
                                "source": "out => out.split('\n')"
                            }
                        }]
                    }]
                }"#,
            ),
        ]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Warn),
            "post_process with empty script/script_template argv must Warn, got: {}",
            result.message
        );
        assert!(
            result.message.contains("pp_empty_script")
                && result.message.contains("pp_empty_template"),
            "must name affected specs: {}",
            result.message
        );
    }

    /// Companion: `post_process` with a `script_template` (rather than
    /// `script`) is also dispatchable — the engine treats the two as
    /// interchangeable inputs to the post-processor.
    #[test]
    fn doctor_passes_when_post_process_has_script_template() {
        let (store, _dir) = store_from_json_fixtures(&[(
            "pp_template.json",
            r#"{
                "name": "pp_template",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "script_template": ["echo {current_token}"],
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "post_process",
                            "source": "out => out.split('\n')"
                        }
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(
            matches!(result.severity, Severity::Ok),
            "post_process with script_template must be OK, got: {}",
            result.message
        );
    }

    /// Regression guard for comment-1: the user-facing warn message must
    /// enumerate every malformed runtime-metadata class the warn predicate
    /// counts — missing js_runtime metadata, empty source, and post_process
    /// without script/script_template. Unsupported script_function/custom
    /// generators are covered by the OK tests above.
    #[test]
    fn doctor_warn_message_enumerates_all_malformed_classes() {
        let (store, _dir) = store_from_json_fixtures(&[(
            "pp_no_script.json",
            r#"{
                "name": "pp_no_script",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "post_process",
                            "source": "out => out.split('\n')"
                        }
                    }]
                }]
            }"#,
        )]);
        let result = check_embedded_runtime_metadata_for_store(&store);
        assert!(matches!(result.severity, Severity::Warn));
        let msg = &result.message;
        assert!(
            msg.contains("missing js_runtime metadata"),
            "must name the missing-metadata class: {msg}"
        );
        assert!(
            msg.contains("empty `js_runtime.source`"),
            "must name the empty-source class: {msg}"
        );
        assert!(
            msg.contains("`post_process` kind") && msg.contains("`script`/`script_template`"),
            "must name the post_process+missing-script class: {msg}"
        );
        assert!(
            !msg.contains("`script_function`/`custom`"),
            "warn message must not conflate unsupported coverage with malformed metadata: {msg}"
        );
    }
}
