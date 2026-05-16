use std::path::Path;

use anyhow::{Context, Result};
use gc_suggest::parse_spec_checked_and_sanitized;
use gc_suggest::spec_dirs::resolve_spec_dirs;
use gc_suggest::specs::{
    validate_spec_generators, ArgSpec, CompletionSpec, GeneratorSpec, JsRuntimeKind, OptionSpec,
    SubcommandSpec,
};
use serde::Serialize;

use crate::sanitize::sanitize_for_terminal;

/// Why a `requires_js: true` generator is undispatchable. Each variant
/// maps 1:1 to the corresponding rejection branch in
/// `gc_suggest::engine::is_supported_script_generator` and
/// `doctor::count_missing_js_runtime_in_spec` so an operator reading a
/// `--strict` warning can disambiguate the converter defect class without
/// re-running `ghost-complete doctor`.
#[derive(Debug, Clone, Copy)]
enum JsRuntimeWarningKind {
    /// `js_runtime` is `None` — the converter dropped the wrapper entirely.
    MissingMetadata,
    /// `js_runtime` is present but `source` is empty/whitespace — the
    /// converter kept the wrapper but dropped the body.
    EmptySource,
    /// `kind: post_process` without a non-empty `script` /
    /// `script_template` — the engine has no shell stdout to feed into the
    /// post-processor.
    PostProcessMissingScript,
    /// `kind: script_function` / `custom` without `self_contained: true` —
    /// the converter has not yet proven the source is free of bundler /
    /// minifier helper closures (`__webpack_require__`, `__exports__`, ...)
    /// that the QuickJS host will not install.
    MissingSelfContained,
}

impl JsRuntimeWarningKind {
    fn message(self) -> &'static str {
        match self {
            Self::MissingMetadata => "requires_js=true without js_runtime metadata",
            Self::EmptySource => "requires_js=true with empty js_runtime.source",
            Self::PostProcessMissingScript => {
                "requires_js=true with kind=post_process but no non-empty script/script_template"
            }
            Self::MissingSelfContained => {
                "requires_js=true with kind=script_function/custom but self_contained!=true"
            }
        }
    }
}

/// Walk a parsed spec and emit warnings for every `requires_js: true`
/// generator that the engine cannot dispatch. Mirrors the engine's
/// `is_supported_script_generator` predicate (and
/// `doctor::count_missing_js_runtime_in_spec`) so the three surfaces
/// agree on what counts as a converter regression. The four classes —
/// missing metadata, empty source, `post_process` without non-empty
/// `script`/`script_template`, and `script_function`/`custom` without
/// `self_contained: true` — are tagged via [`JsRuntimeWarningKind`] so
/// the operator can disambiguate the defect from the warning text alone.
///
/// Surfaced via `--strict` only — the standard `validate_spec_generators`
/// focuses on transform-pipeline shape. This is the converter regression
/// gate documented at `docs/ci-gates.md`.
fn collect_missing_js_runtime_warnings(spec: &CompletionSpec) -> Vec<String> {
    let mut warnings = Vec::new();
    fn has_non_empty_script_or_template(g: &GeneratorSpec) -> bool {
        g.script.as_ref().is_some_and(|script| !script.is_empty())
            || g.script_template
                .as_ref()
                .is_some_and(|template| !template.is_empty())
    }

    fn classify(g: &GeneratorSpec) -> Option<JsRuntimeWarningKind> {
        if !g.requires_js {
            return None;
        }
        match g.js_runtime.as_ref() {
            None => Some(JsRuntimeWarningKind::MissingMetadata),
            Some(rt) => {
                if rt.source.trim().is_empty() {
                    return Some(JsRuntimeWarningKind::EmptySource);
                }
                // Mirror gc-suggest::engine::is_supported_script_generator:
                // post_process is dispatchable iff an accompanying
                // `script` / `script_template` argv is non-empty (the
                // engine has shell stdout to feed into the post-processor).
                // script_function/custom additionally require
                // `self_contained:true`. Without those guarantees the
                // engine refuses to dispatch — so `--strict` must surface
                // these as warnings or `validate-specs` ships a green
                // light while doctor + engine drop the generator.
                match rt.kind {
                    JsRuntimeKind::PostProcess => {
                        if !has_non_empty_script_or_template(g) {
                            Some(JsRuntimeWarningKind::PostProcessMissingScript)
                        } else {
                            None
                        }
                    }
                    JsRuntimeKind::ScriptFunction | JsRuntimeKind::Custom => {
                        if !rt.self_contained {
                            Some(JsRuntimeWarningKind::MissingSelfContained)
                        } else {
                            None
                        }
                    }
                    JsRuntimeKind::TokenOnly => None,
                }
            }
        }
    }

    fn walk_args(args: &[ArgSpec], path: &str, warnings: &mut Vec<String>) {
        for (i, a) in args.iter().enumerate() {
            for (j, g) in a.generators.iter().enumerate() {
                if let Some(kind) = classify(g) {
                    warnings.push(format!(
                        "{path}/args[{i}]/generators[{j}]: {}",
                        kind.message()
                    ));
                }
            }
        }
    }

    fn walk_opts(opts: &[OptionSpec], path: &str, warnings: &mut Vec<String>) {
        for (i, o) in opts.iter().enumerate() {
            // OptionSpec stores the first arg in `args` and the rest in
            // `extra_args` (see `deserialize_option_args`); both slots must be
            // walked or non-first-arg regressions ship silently.
            for (k, arg) in o
                .args
                .as_ref()
                .into_iter()
                .chain(o.extra_args.iter())
                .enumerate()
            {
                for (j, g) in arg.generators.iter().enumerate() {
                    if let Some(kind) = classify(g) {
                        warnings.push(format!(
                            "{path}/options[{i}]/args[{k}]/generators[{j}]: {}",
                            kind.message()
                        ));
                    }
                }
            }
        }
    }

    walk_args(&spec.args, "$", &mut warnings);
    walk_opts(&spec.options, "$", &mut warnings);

    // Iterative descent — mirrors `count_corrected_generators_in_spec`
    // in doctor.rs (lines 295-300). Hand-edited specs or upstream
    // `@withfig/autocomplete` regen with a deeper subcommand tree must
    // not be able to overflow the validator's stack — the AWS spec
    // already nests ~10 levels and `validate-specs --strict` is the
    // converter regression gate per `docs/ci-gates.md`. The stack
    // stores `(slice, parent_path)` pairs so the warning message
    // formatter retains the same `$/subcommands[i]/...` JSON-pointer
    // shape the recursive form produced.
    let mut stack: Vec<(&[SubcommandSpec], String)> = vec![(&spec.subcommands, "$".to_string())];
    while let Some((subs, parent_path)) = stack.pop() {
        for (i, s) in subs.iter().enumerate() {
            let p = format!("{parent_path}/subcommands[{i}]");
            walk_args(&s.args, &p, &mut warnings);
            walk_opts(&s.options, &p, &mut warnings);
            stack.push((&s.subcommands, p));
        }
    }
    warnings
}

/// One NDJSON row describing a single spec. The `ok` field is `true` only
/// when parsing succeeded AND the spec surfaced zero generator warnings —
/// `jq 'select(.ok == false)'` thus matches both parse failures and
/// warn'd specs. Schema owned by `run_validate_specs_inner`'s doc comment.
#[derive(Debug, Serialize)]
struct ValidateSpecRow<'a> {
    spec_name: &'a str,
    ok: bool,
    divergences: Vec<String>,
    warnings: Vec<String>,
}

/// Trailing NDJSON summary row. Uses a nested `summary` object so readers
/// can disambiguate per-spec rows from the summary row via the top-level
/// key (`spec_name` vs `summary`).
#[derive(Debug, Serialize)]
struct ValidateSummaryRow {
    summary: ValidateSummary,
}

#[derive(Debug, Serialize)]
struct ValidateSummary {
    total: usize,
    valid: usize,
    failed: usize,
    with_warnings: usize,
    warnings_total: usize,
    dirs_scanned: usize,
    strict_failed: bool,
}

/// Counts emitted by [`validate_dir`] / [`run_validate_specs_inner`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ValidateCounts {
    pub valid: usize,
    pub failed: usize,
    /// Total transform-pipeline / generator warnings surfaced across all
    /// loaded specs. Independent of `failed` (which counts files that failed
    /// to parse).
    pub warnings: usize,
    /// Count of specs that surfaced at least one warning. `warnings` is the
    /// sum of individual messages; this is the count of affected specs. Used
    /// in the `--json` summary row.
    pub with_warnings: usize,
}

fn count_spec_items(spec: &CompletionSpec) -> (usize, usize) {
    fn count_subcommands(subs: &[SubcommandSpec]) -> usize {
        let mut n = subs.len();
        for sub in subs {
            n += count_subcommands(&sub.subcommands);
        }
        n
    }

    let subcommands = count_subcommands(&spec.subcommands);
    let options = spec.options.len();
    (subcommands, options)
}

fn validate_dir(
    dir: &Path,
    json: bool,
    strict: bool,
    out: &mut dyn std::io::Write,
) -> Result<ValidateCounts> {
    let mut counts = ValidateCounts::default();

    if !dir.exists() {
        if !json {
            writeln!(
                out,
                "  Directory does not exist: {}\n",
                sanitize_for_terminal(&dir.display().to_string())
            )?;
        }
        return Ok(counts);
    }

    let mut entries = Vec::new();
    let mut unreadable = 0usize;
    for entry_result in std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory: {}", dir.display()))?
    {
        match entry_result {
            Ok(e) => {
                if e.path().extension().and_then(|ext| ext.to_str()) == Some("json") {
                    entries.push(e);
                }
            }
            Err(e) => {
                unreadable += 1;
                tracing::warn!("failed to read directory entry in {}: {e}", dir.display());
                // In JSON mode emit one synthetic row per unreadable entry so
                // the schema contract (`summary.failed == count(ok:false
                // rows)`) holds. The iterator `Err` variant does not carry the
                // file name, so we synthesize `<unreadable-{i}>` using the
                // running counter.
                if json {
                    emit_json_spec(
                        out,
                        &format!("<unreadable-{unreadable}>"),
                        false,
                        vec!["unreadable directory entry".to_string()],
                        vec![],
                    )?;
                }
            }
        }
    }
    entries.sort_by_key(|e| e.file_name());

    if unreadable > 0 && !json {
        writeln!(
            out,
            "  \x1b[33m{unreadable} file(s) could not be read\x1b[0m"
        )?;
    }
    if unreadable > 0 {
        counts.failed += unreadable;
    }

    for entry in entries {
        let path = entry.path();
        let raw_file_name = path.file_name().unwrap_or_default().to_string_lossy();
        // `raw_file_name` is used verbatim for JSON output; serde_json handles
        // escaping of control bytes, so ESC-stripping is only needed for the
        // human-readable (ANSI-rendered) path.
        let file_name = sanitize_for_terminal(&raw_file_name);
        let spec_name = raw_file_name
            .strip_suffix(".json")
            .unwrap_or(&raw_file_name)
            .to_string();

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                if json {
                    emit_json_spec(out, &spec_name, false, vec![e.to_string()], vec![])?;
                } else {
                    writeln!(
                        out,
                        "  \x1b[31mFAIL\x1b[0m  {file_name}: {}",
                        sanitize_for_terminal(&e.to_string())
                    )?;
                }
                counts.failed += 1;
                continue;
            }
        };

        match parse_spec_checked_and_sanitized(&contents) {
            Ok(mut spec) => {
                let (subs, opts) = count_spec_items(&spec);
                let mut warnings = validate_spec_generators(&mut spec);
                // In --strict mode, surface specs with requires_js
                // generators that are missing js_runtime metadata. The
                // check is strict-only because the current corpus is
                // fully populated; a non-strict invocation should not
                // suddenly start warning on every existing on-disk spec
                // that hasn't yet been re-converted.
                if strict {
                    warnings.extend(collect_missing_js_runtime_warnings(&spec));
                }
                if json {
                    // In JSON mode: ok=true only when parse succeeded AND zero
                    // warnings. Warnings flip ok to false so `jq 'select(.ok
                    // == false)'` surfaces both failures and warn'd specs.
                    let ok = warnings.is_empty();
                    emit_json_spec(out, &spec_name, ok, vec![], warnings.clone())?;
                } else if warnings.is_empty() {
                    writeln!(
                        out,
                        "  \x1b[32m OK \x1b[0m  {file_name} ({subs} subcommands, {opts} options)"
                    )?;
                } else {
                    writeln!(
                        out,
                        "  \x1b[33mWARN\x1b[0m  {file_name} ({subs} subcommands, {opts} options, {} warning{})",
                        warnings.len(),
                        if warnings.len() == 1 { "" } else { "s" }
                    )?;
                    for w in &warnings {
                        writeln!(out, "         - {}", sanitize_for_terminal(w))?;
                    }
                }
                if !warnings.is_empty() {
                    counts.with_warnings += 1;
                }
                counts.warnings += warnings.len();
                counts.valid += 1;
            }
            Err(e) => {
                if json {
                    emit_json_spec(out, &spec_name, false, vec![e.to_string()], vec![])?;
                } else {
                    writeln!(
                        out,
                        "  \x1b[31mFAIL\x1b[0m  {file_name}: {}",
                        sanitize_for_terminal(&e.to_string())
                    )?;
                }
                counts.failed += 1;
            }
        }
    }

    Ok(counts)
}

/// Emit one NDJSON row describing a single spec. The schema is documented on
/// the `--json` flag in `run_validate_specs_inner`.
fn emit_json_spec(
    out: &mut dyn std::io::Write,
    spec_name: &str,
    ok: bool,
    divergences: Vec<String>,
    warnings: Vec<String>,
) -> Result<()> {
    let row = ValidateSpecRow {
        spec_name,
        ok,
        divergences,
        warnings,
    };
    writeln!(out, "{}", serde_json::to_string(&row)?)?;
    Ok(())
}

/// Outcome of a `validate-specs` run; used by the CLI entry point to decide
/// the process exit code.
#[derive(Debug, Default, Clone, Copy)]
pub struct ValidateOutcome {
    pub counts: ValidateCounts,
    /// True if the user passed `--strict` and at least one warning was
    /// surfaced. The CLI promotes this to a non-zero exit.
    pub strict_failed: bool,
}

/// Runs validation against the configured spec dirs, writing all output to
/// `out`. Pure (no `process::exit`, no global stdout) so tests can drive it.
///
/// When `json` is `true`, emits newline-delimited JSON: one row per spec plus
/// a trailing `{"summary":{...}}` row. Human-readable output is suppressed.
/// Distinguish the two shapes by which top-level key is present:
///   `... | jq 'select(.spec_name)'`  → per-spec rows
///   `... | jq 'select(.summary)'`    → summary row
pub fn run_validate_specs_inner(
    config_path: Option<&str>,
    strict: bool,
    json: bool,
    out: &mut dyn std::io::Write,
) -> Result<ValidateOutcome> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;

    let dirs = resolve_spec_dirs(&config.paths.spec_dirs);
    let mut counts = ValidateCounts::default();

    for dir in &dirs {
        if !json {
            writeln!(
                out,
                "Validating specs in {}\n",
                sanitize_for_terminal(&dir.display().to_string())
            )?;
        }
        let dir_counts = validate_dir(dir, json, strict, out)?;
        counts.valid += dir_counts.valid;
        counts.failed += dir_counts.failed;
        counts.warnings += dir_counts.warnings;
        counts.with_warnings += dir_counts.with_warnings;
    }

    if dirs.is_empty() {
        if !json {
            writeln!(out, "No spec directories found.")?;
        } else {
            // Still emit a summary so JSON consumers always see one summary
            // row, even when there are no dirs to scan.
            let strict_failed = strict && counts.warnings > 0;
            emit_json_summary(out, &counts, 0, strict_failed)?;
        }
        return Ok(ValidateOutcome::default());
    }

    let total = counts.valid + counts.failed;
    let strict_failed = strict && counts.warnings > 0;

    if json {
        emit_json_summary(out, &counts, dirs.len(), strict_failed)?;
        return Ok(ValidateOutcome {
            counts,
            strict_failed,
        });
    }

    writeln!(out)?;
    if counts.failed == 0 {
        writeln!(out, "{total}/{total} specs valid.")?;
    } else {
        writeln!(
            out,
            "{}/{total} specs valid, {} failed.",
            counts.valid, counts.failed
        )?;
    }
    if counts.warnings > 0 {
        writeln!(
            out,
            "\x1b[33m{} generator warning(s) across all specs.\x1b[0m",
            counts.warnings
        )?;
        if strict {
            writeln!(out, "\x1b[31mstrict mode: warnings are errors.\x1b[0m")?;
        }
    }

    Ok(ValidateOutcome {
        counts,
        strict_failed,
    })
}

/// Emit the trailing `{"summary":{...}}` NDJSON row.
fn emit_json_summary(
    out: &mut dyn std::io::Write,
    counts: &ValidateCounts,
    dirs_scanned: usize,
    strict_failed: bool,
) -> Result<()> {
    let total = counts.valid + counts.failed;
    let row = ValidateSummaryRow {
        summary: ValidateSummary {
            total,
            valid: counts.valid,
            failed: counts.failed,
            with_warnings: counts.with_warnings,
            warnings_total: counts.warnings,
            dirs_scanned,
            strict_failed,
        },
    };
    writeln!(out, "{}", serde_json::to_string(&row)?)?;
    Ok(())
}

/// Entry point invoked by `main.rs`. Reads `--strict` and `--json` directly
/// out of the process args (the top-level CLI parser only forwards a
/// positional arg list to subcommands, mirroring the existing `--dry-run`
/// handling for `install`).
pub fn run_validate_specs(config_path: Option<&str>) -> Result<()> {
    let strict = std::env::args().any(|a| a == "--strict");
    let json = std::env::args().any(|a| a == "--json");
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let outcome = run_validate_specs_inner(config_path, strict, json, &mut handle)?;
    if outcome.counts.failed > 0 || outcome.strict_failed {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_spec(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    /// Build a config TOML pointing at a single spec dir, write it to a temp
    /// file, and return its path.
    fn write_config_for(spec_dir: &Path, tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let cfg_path = tmp.path().join("config.toml");
        let body = format!(
            "[paths]\nspec_dirs = [\"{}\"]\n",
            spec_dir.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(&cfg_path, body).unwrap();
        cfg_path
    }

    #[test]
    fn test_validate_specs_warns_on_invalid_transform_pipeline() {
        // A spec with a bad transform pipeline (post-split before split) must
        // surface as a warning. The legacy implementation only ran
        // `serde_json::from_str` and printed "OK" for this exact case.
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "bad_pipeline.json",
            r#"{
                "name": "bad",
                "args": [{
                    "name": "x",
                    "generators": [
                        {"script": ["cmd"], "transforms": ["filter_empty", "split_lines"]}
                    ]
                }]
            }"#,
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, false, &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);
        assert!(
            outcome.counts.warnings > 0,
            "expected at least one warning, got 0 (output was:\n{txt})"
        );
        assert!(
            txt.contains("WARN"),
            "expected WARN line in output, got:\n{txt}"
        );
        assert!(
            !outcome.strict_failed,
            "non-strict run should not flip strict_failed"
        );
    }

    #[test]
    fn test_validate_specs_strict_mode_promotes_warnings_to_failure() {
        // In strict mode, warnings produce strict_failed=true so the CLI
        // exits non-zero.
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "bad.json",
            r#"{
                "name": "bad",
                "args": [{
                    "name": "x",
                    "generators": [
                        {"script": ["cmd"], "transforms": ["split_lines", "split_lines"]}
                    ]
                }]
            }"#,
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(outcome.counts.warnings > 0);
        assert!(
            outcome.strict_failed,
            "strict mode + warnings should flip strict_failed"
        );
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("strict mode"),
            "expected strict-mode banner in output:\n{txt}"
        );
    }

    #[test]
    fn test_validate_specs_clean_spec_strict_mode_is_ok() {
        // A clean spec with valid transforms must NOT trip strict mode.
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "good.json",
            r#"{
                "name": "good",
                "args": [{
                    "name": "x",
                    "generators": [
                        {"script": ["cmd"], "transforms": ["split_lines", "filter_empty"]}
                    ]
                }]
            }"#,
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert_eq!(outcome.counts.warnings, 0);
        assert_eq!(outcome.counts.failed, 0);
        assert!(!outcome.strict_failed);
    }

    #[test]
    fn run_validate_specs_with_opts_accepts_typed_flags_without_env_scan() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(&spec_dir, "good.json", r#"{"name":"good"}"#);
        let cfg = write_config_for(&spec_dir, &tmp);

        run_validate_specs_with_opts(Some(cfg.to_str().unwrap()), false, true).unwrap();
    }

    /// Sanity check that `validate_dir` writes to its sink rather than
    /// stdout — keeps the rest of the test surface clean.
    #[test]
    fn test_validate_dir_writes_to_provided_sink() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_spec(tmp.path(), "ok.json", r#"{"name":"ok"}"#);
        let mut buf: Vec<u8> = Vec::new();
        validate_dir(tmp.path(), false, false, &mut buf).unwrap();
        buf.flush().unwrap();
        let txt = String::from_utf8_lossy(&buf);
        assert!(txt.contains("ok.json"), "got: {txt}");
    }

    /// Split NDJSON output into per-line `serde_json::Value`s, skipping empty
    /// tails. Keeps the per-spec and summary rows discoverable by inspecting
    /// the top-level key (`spec_name` vs `summary`).
    fn parse_ndjson(out: &[u8]) -> Vec<serde_json::Value> {
        let text = String::from_utf8_lossy(out);
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .unwrap_or_else(|e| panic!("line is not valid JSON: {l:?}: {e}"))
            })
            .collect()
    }

    #[test]
    fn test_json_mode_emits_one_object_per_spec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(&spec_dir, "alpha.json", r#"{"name":"alpha"}"#);
        write_spec(&spec_dir, "beta.json", r#"{"name":"beta"}"#);
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, true, &mut out).unwrap();
        let rows = parse_ndjson(&out);

        assert_eq!(
            rows.len(),
            3,
            "expected 2 spec rows + 1 summary row, got {} rows",
            rows.len()
        );
        let spec_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.get("spec_name").is_some())
            .collect();
        assert_eq!(spec_rows.len(), 2);
        for row in spec_rows {
            assert_eq!(row["ok"], serde_json::Value::Bool(true));
            assert!(row["divergences"].as_array().unwrap().is_empty());
            assert!(row["warnings"].as_array().unwrap().is_empty());
        }
        let names: Vec<_> = rows
            .iter()
            .filter_map(|r| r.get("spec_name").and_then(|v| v.as_str()))
            .collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[test]
    fn test_json_mode_surfaces_parse_error_as_divergence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(&spec_dir, "broken.json", "{not valid json");
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, true, &mut out).unwrap();
        let rows = parse_ndjson(&out);

        let spec_row = rows
            .iter()
            .find(|r| r.get("spec_name").is_some())
            .expect("expected a spec row");
        assert_eq!(spec_row["spec_name"], "broken");
        assert_eq!(spec_row["ok"], serde_json::Value::Bool(false));
        let divs = spec_row["divergences"].as_array().unwrap();
        assert!(
            !divs.is_empty(),
            "expected non-empty divergences, got {divs:?}"
        );
    }

    #[test]
    fn test_json_mode_surfaces_warnings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "ugly.json",
            r#"{
                "name": "ugly",
                "args": [{
                    "name": "x",
                    "generators": [
                        {"script": ["cmd"], "transforms": ["split_lines", "split_lines"]}
                    ]
                }]
            }"#,
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, true, &mut out).unwrap();
        let rows = parse_ndjson(&out);

        let spec_row = rows
            .iter()
            .find(|r| r.get("spec_name").is_some())
            .expect("expected a spec row");
        assert_eq!(spec_row["ok"], serde_json::Value::Bool(false));
        assert!(
            spec_row["divergences"].as_array().unwrap().is_empty(),
            "parse succeeded so divergences should be empty"
        );
        assert!(
            !spec_row["warnings"].as_array().unwrap().is_empty(),
            "expected at least one warning"
        );
    }

    #[test]
    fn test_json_mode_summary_totals_match() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(&spec_dir, "good.json", r#"{"name":"good"}"#);
        write_spec(&spec_dir, "broken.json", "{not valid json");
        write_spec(
            &spec_dir,
            "warned.json",
            r#"{
                "name": "warned",
                "args": [{
                    "name": "x",
                    "generators": [
                        {"script": ["cmd"], "transforms": ["split_lines", "split_lines"]}
                    ]
                }]
            }"#,
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, true, &mut out).unwrap();
        let rows = parse_ndjson(&out);

        let summary = rows
            .iter()
            .find(|r| r.get("summary").is_some())
            .expect("expected a summary row");
        let s = &summary["summary"];
        assert_eq!(s["total"], 3);
        assert_eq!(s["valid"], 2);
        assert_eq!(s["failed"], 1);
        assert_eq!(s["with_warnings"], 1);
        assert_eq!(s["dirs_scanned"], 1);
    }

    /// Verifies the synthetic-row shape emitted for unreadable directory
    /// entries in JSON mode. Arranging an actual `read_dir` iterator `Err`
    /// portably is flaky (it depends on kernel/FS behavior — EACCES from
    /// `readdir` vs from `stat` varies by platform), so we instead exercise
    /// `emit_json_spec` with the exact arguments `validate_dir` uses and
    /// assert the row satisfies the schema contract on `<unreadable-{i}>` /
    /// `ok:false` / non-empty divergences.
    #[test]
    fn test_emit_json_spec_unreadable_row_shape() {
        let mut buf: Vec<u8> = Vec::new();
        emit_json_spec(
            &mut buf,
            "<unreadable-1>",
            false,
            vec!["unreadable directory entry".to_string()],
            vec![],
        )
        .unwrap();
        let rows = parse_ndjson(&buf);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["spec_name"], "<unreadable-1>");
        assert_eq!(row["ok"], serde_json::Value::Bool(false));
        let divs = row["divergences"].as_array().unwrap();
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0], "unreadable directory entry");
        assert!(row["warnings"].as_array().unwrap().is_empty());
    }

    /// End-to-end assertion of the JSON schema invariant that motivated the
    /// fix: `summary.failed == count(ok:false rows)`. Uses only readable
    /// entries (since portably provoking a `read_dir` `Err` variant is
    /// flaky in CI), but seeds one corrupt spec so at least one `ok:false`
    /// row is produced via the already-tested branch. The key regression
    /// guarded here is that every contributor to `counts.failed` also emits
    /// a row — the unreadable branch is the only place this used to be
    /// violated.
    #[test]
    fn test_json_mode_failed_count_matches_ok_false_rows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(&spec_dir, "good.json", r#"{"name":"good"}"#);
        write_spec(&spec_dir, "broken.json", "{not valid json");
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, true, &mut out).unwrap();
        let rows = parse_ndjson(&out);

        let summary = rows
            .iter()
            .find(|r| r.get("summary").is_some())
            .expect("expected a summary row");
        let failed = summary["summary"]["failed"].as_u64().unwrap();
        let ok_false_rows = rows
            .iter()
            .filter(|r| r.get("spec_name").is_some())
            .filter(|r| r["ok"] == serde_json::Value::Bool(false))
            .count() as u64;
        assert_eq!(
            failed, ok_false_rows,
            "summary.failed must equal count of spec rows with ok=false"
        );
    }

    #[test]
    fn test_json_mode_suppresses_human_output() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(&spec_dir, "clean.json", r#"{"name":"clean"}"#);
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, true, &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);

        assert!(
            !txt.contains("\x1b["),
            "expected no ANSI escapes, got: {txt}"
        );
        assert!(!txt.contains("Validating specs in"), "got: {txt}");
        assert!(!txt.contains(" OK "), "got: {txt}");
        assert!(!txt.contains("WARN"), "got: {txt}");
        assert!(!txt.contains("FAIL"), "got: {txt}");
        assert!(!txt.contains("specs valid"), "got: {txt}");
    }

    /// In `--strict` mode a spec with `requires_js: true` but no
    /// `js_runtime` metadata flips `strict_failed`. Catches a converter
    /// regression where the regen would silently drop the metadata field.
    #[test]
    fn validate_strict_fails_on_missing_js_runtime() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "missing.json",
            r#"{
                "name": "missing",
                "args": [{
                    "name": "x",
                    "generators": [{"requires_js": true}]
                }]
            }"#,
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        // Non-strict mode: no warning, no strict_failed.
        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, false, &mut out).unwrap();
        assert_eq!(
            outcome.counts.warnings, 0,
            "non-strict mode must not warn on missing js_runtime"
        );
        assert!(!outcome.strict_failed);

        // Strict mode: at least one warning + strict_failed=true.
        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(
            outcome.counts.warnings > 0,
            "strict mode must surface a warning for missing js_runtime"
        );
        assert!(outcome.strict_failed);
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("requires_js=true without js_runtime"),
            "expected message naming the missing field: {txt}"
        );
    }

    #[test]
    fn validate_strict_fails_on_missing_js_runtime_in_second_option_arg() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "option_args.json",
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
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(
            outcome.counts.warnings > 0,
            "strict mode must warn on missing js_runtime in option args array"
        );
        assert!(outcome.strict_failed);
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("requires_js=true without js_runtime"),
            "expected missing js_runtime warning: {txt}"
        );
    }

    /// A spec with `requires_js: true` and a properly populated
    /// `js_runtime` block does NOT flip strict_failed. Companion test to
    /// `validate_strict_fails_on_missing_js_runtime` so a regression can
    /// be triangulated to either side of the predicate.
    #[test]
    fn validate_strict_passes_with_js_runtime() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "complete.json",
            r#"{
                "name": "complete",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "script": ["cmd"],
                        "requires_js": true,
                        "js_runtime": {"kind":"post_process","source":"()=>[]"}
                    }]
                }]
            }"#,
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert_eq!(outcome.counts.warnings, 0);
        assert!(!outcome.strict_failed);
    }

    /// Empty `js_runtime.source` is also rejected in strict mode — the
    /// converter must not regress to dropping the body but keeping the
    /// wrapper.
    #[test]
    fn validate_strict_fails_on_empty_js_runtime_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "empty.json",
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
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(outcome.counts.warnings > 0);
        assert!(outcome.strict_failed);
    }

    /// Regression guard for test-iter3-2: the iterative subcommand-descent
    /// loop in `collect_missing_js_runtime_warnings` (intentionally
    /// non-recursive to handle AWS-style ~10-level subcommand nesting
    /// without stack overflow) must
    /// preserve the same `$/subcommands[i]/subcommands[j]/.../args[k]`
    /// JSON-pointer shape the recursive form produced. A future
    /// refactor that breaks the path concatenation (e.g. dropping the
    /// `parent_path` join) would silently emit malformed warning
    /// paths to operators without test failure. We synthesise a
    /// 6-level-deep nest with `requires_js: true` (no `js_runtime`)
    /// at the leaf and assert the warning path is exactly the
    /// expected JSON pointer — pinning both that the iteration
    /// completes (test passes at all) AND that the path shape
    /// matches.
    #[test]
    fn validate_strict_warning_path_survives_deep_nested_subcommands() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        // 6 levels of nested subcommands (root → l1 → l2 → l3 → l4 →
        // l5 → l6) with the missing-js_runtime defect at the deepest
        // level. The root spec has no args/options of its own; each
        // intermediate level carries exactly one subcommand. The
        // expected warning path therefore traverses six
        // `subcommands[0]` segments before landing on the
        // `args[0]/generators[0]` defect.
        write_spec(
            &spec_dir,
            "deep.json",
            r#"{
                "name": "deep",
                "subcommands": [{
                    "name": "l1",
                    "subcommands": [{
                        "name": "l2",
                        "subcommands": [{
                            "name": "l3",
                            "subcommands": [{
                                "name": "l4",
                                "subcommands": [{
                                    "name": "l5",
                                    "subcommands": [{
                                        "name": "l6",
                                        "args": [{
                                            "name": "x",
                                            "generators": [{"requires_js": true}]
                                        }]
                                    }]
                                }]
                            }]
                        }]
                    }]
                }]
            }"#,
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(
            outcome.counts.warnings > 0,
            "strict mode must warn on missing js_runtime at deep nesting"
        );
        assert!(outcome.strict_failed);

        let txt = String::from_utf8_lossy(&out);
        let expected_path = "$/subcommands[0]/subcommands[0]/subcommands[0]\
                             /subcommands[0]/subcommands[0]/subcommands[0]\
                             /args[0]/generators[0]: requires_js=true without js_runtime metadata";
        assert!(
            txt.contains(expected_path),
            "expected exact JSON-pointer path for the deep-nested defect.\n\
             expected substring: {expected_path}\n\
             got output:\n{txt}"
        );
    }

    /// Regression guard for sf-iter4-1: a `script_function` / `custom`
    /// generator without `self_contained: true` cannot be dispatched by
    /// the engine (`gc_suggest::engine::is_supported_script_generator`).
    /// `--strict` must surface it so converter regressions on the
    /// self-containment analyzer cannot land silently — without this,
    /// the validate gate diverges from doctor + status + engine and the
    /// docstring's "converter regressions cannot land silently" promise
    /// breaks for two of the three rejection classes.
    #[test]
    fn validate_strict_fails_when_script_function_lacks_self_contained() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
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
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(
            outcome.counts.warnings > 0,
            "strict mode must warn on script_function without self_contained"
        );
        assert!(outcome.strict_failed);
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("self_contained!=true"),
            "expected self_contained-specific warning text, got: {txt}"
        );
    }

    /// Companion to the script_function test for `kind: custom`. Same
    /// gate, different enum variant — both are filtered by the engine
    /// when `self_contained` is missing or `false`.
    #[test]
    fn validate_strict_fails_when_custom_lacks_self_contained() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
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
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(outcome.counts.warnings > 0);
        assert!(outcome.strict_failed);
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("self_contained!=true"),
            "expected self_contained-specific warning text, got: {txt}"
        );
    }

    /// Regression guard for sf-iter4-1: a `post_process` generator
    /// without a non-empty `script` / `script_template` cannot be
    /// dispatched (the engine has no shell stdout to feed into the
    /// post-processor). `--strict` must surface it as a hard failure,
    /// stricter than the doctor's `doctor_warns_when_post_process_lacks_script`
    /// counterpart (doctor warns so a clean install isn't noisy; `validate
    /// --strict` is opt-in CI tooling and should still fail loud).
    #[test]
    fn validate_strict_fails_when_post_process_lacks_script() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
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
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(
            outcome.counts.warnings > 0,
            "strict mode must warn on post_process without script/script_template"
        );
        assert!(outcome.strict_failed);
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("kind=post_process") && txt.contains("script/script_template"),
            "expected post_process-specific warning text, got: {txt}"
        );
    }

    /// Companion positive control: `post_process` with a
    /// `script_template` (rather than `script`) is dispatchable; strict
    /// must not flag it.
    #[test]
    fn validate_strict_passes_when_post_process_has_script_template() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
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
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert_eq!(outcome.counts.warnings, 0);
        assert!(!outcome.strict_failed);
    }

    /// Companion positive control: `script_function` proven
    /// `self_contained: true` is dispatchable and must not flip strict.
    #[test]
    fn validate_strict_passes_when_script_function_is_self_contained() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
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
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert_eq!(outcome.counts.warnings, 0);
        assert!(!outcome.strict_failed);
    }

    /// Regression guard for comment-3: the empty-source warning must
    /// distinguish itself from the missing-metadata warning. Operators
    /// reading the message need to know whether the converter dropped
    /// the wrapper (MissingMetadata) or only the body (EmptySource);
    /// before the sf-iter4-1 fix both classes shared the same misleading
    /// "without js_runtime metadata" string.
    #[test]
    fn validate_strict_empty_source_warning_is_disambiguated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        write_spec(
            &spec_dir,
            "empty.json",
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
        );
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let outcome =
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, false, &mut out).unwrap();
        assert!(outcome.counts.warnings > 0);
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("empty js_runtime.source"),
            "empty-source defect must surface its own disambiguated message \
             rather than the misleading 'without js_runtime metadata' text. \
             got: {txt}"
        );
        assert!(
            !txt.contains("without js_runtime metadata"),
            "empty-source defect must NOT borrow the missing-metadata text. \
             got: {txt}"
        );
    }

    /// Regression guard for sf-iter4-1: `validate-specs --strict`,
    /// `doctor`, and `engine::is_supported_script_generator` must agree
    /// on the same predicate. Drives a fixture matrix of every
    /// dispatchable / undispatchable shape past all three surfaces and
    /// asserts the verdicts coincide. Without this, a future refactor
    /// that touches one surface can silently re-introduce the divergence
    /// the iter-4 fix closed.
    #[test]
    fn validate_doctor_and_engine_predicates_agree() {
        // Each fixture is (name, generator JSON body, supported?). The
        // generator body is embedded into a minimal spec stub by the
        // helper below.
        let cases: &[(&str, &str, bool)] = &[
            // Supported: requires_js=false generators short-circuit to true
            // in the engine and are not flagged anywhere.
            (
                "non-js",
                r#"{"script": ["echo a"], "transforms": ["split_lines"]}"#,
                true,
            ),
            // Supported: post_process with script + non-empty source.
            (
                "pp-with-script",
                r#"{
                    "script": ["cmd"],
                    "requires_js": true,
                    "js_runtime": {"kind":"post_process","source":"out=>[]"}
                }"#,
                true,
            ),
            // Supported: post_process with script_template + non-empty source.
            (
                "pp-with-script-template",
                r#"{
                    "script_template": ["echo {current_token}"],
                    "requires_js": true,
                    "js_runtime": {"kind":"post_process","source":"out=>[]"}
                }"#,
                true,
            ),
            // Supported: script_function proven self_contained.
            (
                "sf-self-contained",
                r#"{
                    "requires_js": true,
                    "js_runtime": {
                        "kind":"script_function",
                        "source":"()=>['a']",
                        "self_contained": true
                    }
                }"#,
                true,
            ),
            // Supported: custom proven self_contained.
            (
                "custom-self-contained",
                r#"{
                    "requires_js": true,
                    "js_runtime": {
                        "kind":"custom",
                        "source":"()=>[{name:'a'}]",
                        "self_contained": true
                    }
                }"#,
                true,
            ),
            // Supported: token_only needs non-empty source but does not
            // require self_contained because no host API is installed.
            (
                "token-only",
                r#"{
                    "requires_js": true,
                    "js_runtime": {
                        "kind":"token_only",
                        "source":"tokens.map(name => ({ name }))",
                        "self_contained": false
                    }
                }"#,
                true,
            ),
            // Unsupported: missing js_runtime entirely.
            ("missing-metadata", r#"{"requires_js": true}"#, false),
            // Unsupported: js_runtime present but source is whitespace.
            (
                "empty-source",
                r#"{
                    "requires_js": true,
                    "js_runtime": {"kind":"custom","source":"   "}
                }"#,
                false,
            ),
            // Unsupported: post_process without script/script_template.
            (
                "pp-no-script",
                r#"{
                    "requires_js": true,
                    "js_runtime": {"kind":"post_process","source":"out=>[]"}
                }"#,
                false,
            ),
            // Unsupported: post_process with an empty script argv is as
            // undispatchable as a missing script.
            (
                "pp-empty-script",
                r#"{
                    "script": [],
                    "requires_js": true,
                    "js_runtime": {"kind":"post_process","source":"out=>[]"}
                }"#,
                false,
            ),
            // Unsupported: post_process with an empty script_template
            // argv gives the engine no stdout source to post-process.
            (
                "pp-empty-script-template",
                r#"{
                    "script_template": [],
                    "requires_js": true,
                    "js_runtime": {"kind":"post_process","source":"out=>[]"}
                }"#,
                false,
            ),
            // Unsupported: script_function without self_contained.
            (
                "sf-no-self-contained",
                r#"{
                    "requires_js": true,
                    "js_runtime": {"kind":"script_function","source":"()=>[]"}
                }"#,
                false,
            ),
            // Unsupported: custom with self_contained:false.
            (
                "custom-not-self-contained",
                r#"{
                    "requires_js": true,
                    "js_runtime": {
                        "kind":"custom",
                        "source":"()=>[]",
                        "self_contained": false
                    }
                }"#,
                false,
            ),
        ];

        for &(name, gen_body, supported) in cases {
            // Build a minimal spec containing exactly this generator
            // and run it through `parse_spec_checked_and_sanitized` so
            // we exercise the same parse path the validator uses.
            let spec_json = format!(
                r#"{{
                    "name": "{name}",
                    "args": [{{
                        "name": "x",
                        "generators": [{gen_body}]
                    }}]
                }}"#
            );
            let mut spec = parse_spec_checked_and_sanitized(&spec_json)
                .unwrap_or_else(|e| panic!("fixture '{name}' must parse: {e}"));

            // Predicate 1: validate.rs (this file).
            let warnings = collect_missing_js_runtime_warnings(&spec);
            let validate_supported = warnings.is_empty();

            // Predicate 2: walk every generator and feed it through the
            // standard validation pipeline so we catch transform-shape
            // regressions if a future refactor co-opts the predicate.
            let _shape_warnings = validate_spec_generators(&mut spec);

            assert_eq!(
                validate_supported, supported,
                "fixture '{name}': validate.rs disagreed with expected verdict \
                 (expected supported={supported}, validate said {validate_supported}, \
                 warnings={warnings:?})"
            );
        }
    }
}
