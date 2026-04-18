use std::path::Path;

use anyhow::{Context, Result};
use gc_suggest::parse_spec_checked_and_sanitized;
use gc_suggest::spec_dirs::resolve_spec_dirs;
use gc_suggest::specs::{validate_spec_generators, CompletionSpec, SubcommandSpec};

/// Strip control characters from text headed for the user's terminal.
/// File paths, spec names, and error strings all reach `writeln!`
/// unescaped; any of them could smuggle in CSI/OSC sequences without
/// this guard. Mirrors `sanitize_text` inside `gc-suggest`.
fn sanitize_for_terminal(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
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

fn validate_dir(dir: &Path, out: &mut dyn std::io::Write) -> Result<ValidateCounts> {
    let mut counts = ValidateCounts::default();

    if !dir.exists() {
        writeln!(
            out,
            "  Directory does not exist: {}\n",
            sanitize_for_terminal(&dir.display().to_string())
        )?;
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

    if unreadable > 0 {
        writeln!(
            out,
            "  \x1b[33m{unreadable} file(s) could not be read\x1b[0m"
        )?;
        counts.failed += unreadable;
    }

    for entry in entries {
        let path = entry.path();
        let raw_file_name = path.file_name().unwrap_or_default().to_string_lossy();
        let file_name = sanitize_for_terminal(&raw_file_name);

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                writeln!(
                    out,
                    "  \x1b[31mFAIL\x1b[0m  {file_name}: {}",
                    sanitize_for_terminal(&e.to_string())
                )?;
                counts.failed += 1;
                continue;
            }
        };

        match parse_spec_checked_and_sanitized(&contents) {
            Ok(mut spec) => {
                let (subs, opts) = count_spec_items(&spec);
                let warnings = validate_spec_generators(&mut spec);
                if warnings.is_empty() {
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
                    counts.warnings += warnings.len();
                }
                counts.valid += 1;
            }
            Err(e) => {
                writeln!(
                    out,
                    "  \x1b[31mFAIL\x1b[0m  {file_name}: {}",
                    sanitize_for_terminal(&e.to_string())
                )?;
                counts.failed += 1;
            }
        }
    }

    Ok(counts)
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
pub fn run_validate_specs_inner(
    config_path: Option<&str>,
    strict: bool,
    out: &mut dyn std::io::Write,
) -> Result<ValidateOutcome> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;

    let dirs = resolve_spec_dirs(&config.paths.spec_dirs);
    let mut counts = ValidateCounts::default();

    for dir in &dirs {
        writeln!(
            out,
            "Validating specs in {}\n",
            sanitize_for_terminal(&dir.display().to_string())
        )?;
        let dir_counts = validate_dir(dir, out)?;
        counts.valid += dir_counts.valid;
        counts.failed += dir_counts.failed;
        counts.warnings += dir_counts.warnings;
    }

    if dirs.is_empty() {
        writeln!(out, "No spec directories found.")?;
        return Ok(ValidateOutcome::default());
    }

    let total = counts.valid + counts.failed;
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

    let strict_failed = strict && counts.warnings > 0;
    Ok(ValidateOutcome {
        counts,
        strict_failed,
    })
}

/// Entry point invoked by `main.rs`. Reads `--strict` directly out of the
/// process args (the top-level CLI parser only forwards a positional arg
/// list to subcommands, mirroring the existing `--dry-run` handling for
/// `install`).
pub fn run_validate_specs(config_path: Option<&str>) -> Result<()> {
    let strict = std::env::args().any(|a| a == "--strict");
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let outcome = run_validate_specs_inner(config_path, strict, &mut handle)?;
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
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), false, &mut out).unwrap();
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
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, &mut out).unwrap();
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
            run_validate_specs_inner(Some(cfg.to_str().unwrap()), true, &mut out).unwrap();
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
        validate_dir(tmp.path(), &mut buf).unwrap();
        buf.flush().unwrap();
        let txt = String::from_utf8_lossy(&buf);
        assert!(txt.contains("ok.json"), "got: {txt}");
    }
}
