use std::path::Path;

use anyhow::{Context, Result};
use gc_suggest::parse_spec_checked_and_sanitized;
use gc_suggest::spec_dirs::resolve_spec_dirs;
use gc_suggest::specs::{validate_spec_generators, CompletionSpec, SubcommandSpec};

use crate::sanitize::sanitize_for_terminal;

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

fn validate_dir(dir: &Path, json: bool, out: &mut dyn std::io::Write) -> Result<ValidateCounts> {
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
                let warnings = validate_spec_generators(&mut spec);
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
    let row = serde_json::json!({
        "spec_name": spec_name,
        "ok": ok,
        "divergences": divergences,
        "warnings": warnings,
    });
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
        let dir_counts = validate_dir(dir, json, out)?;
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
    let summary = serde_json::json!({
        "summary": {
            "total": total,
            "valid": counts.valid,
            "failed": counts.failed,
            "with_warnings": counts.with_warnings,
            "warnings_total": counts.warnings,
            "dirs_scanned": dirs_scanned,
            "strict_failed": strict_failed,
        }
    });
    writeln!(out, "{}", serde_json::to_string(&summary)?)?;
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

    /// Sanity check that `validate_dir` writes to its sink rather than
    /// stdout — keeps the rest of the test surface clean.
    #[test]
    fn test_validate_dir_writes_to_provided_sink() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_spec(tmp.path(), "ok.json", r#"{"name":"ok"}"#);
        let mut buf: Vec<u8> = Vec::new();
        validate_dir(tmp.path(), false, &mut buf).unwrap();
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
}
