use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gc_suggest::spec_dirs::resolve_spec_dirs;
use gc_suggest::specs::{ArgSpec, CompletionSpec, GeneratorSpec, OptionSpec, SubcommandSpec};
use gc_suggest::SpecStore;

use crate::sanitize::sanitize_for_terminal;

/// Embedded fallback baseline. Used when no on-disk baseline is discoverable
/// (typical for a user-installed binary where the repo `docs/` directory is
/// not available). Keeps the "Coverage trend" section working out of the box.
const EMBEDDED_BASELINE: &str = include_str!("../../../docs/coverage-baseline.json");

/// Check if a spec tree contains any generators with `requires_js: true`.
fn has_requires_js(spec: &CompletionSpec) -> bool {
    check_args_for_js(&spec.args)
        || check_options_for_js(&spec.options)
        || check_subcommands_for_js(&spec.subcommands)
}

fn check_generators_for_js(generators: &[GeneratorSpec]) -> bool {
    generators.iter().any(|g| g.requires_js)
}

fn check_arg_for_js(arg: &ArgSpec) -> bool {
    check_generators_for_js(&arg.generators)
}

fn check_args_for_js(args: &[ArgSpec]) -> bool {
    args.iter().any(check_arg_for_js)
}

fn check_options_for_js(options: &[OptionSpec]) -> bool {
    options
        .iter()
        .any(|o| o.args.as_ref().is_some_and(check_arg_for_js))
}

fn check_subcommands_for_js(subcommands: &[SubcommandSpec]) -> bool {
    subcommands.iter().any(|s| {
        check_args_for_js(&s.args)
            || check_options_for_js(&s.options)
            || check_subcommands_for_js(&s.subcommands)
    })
}

/// A single release row inside `docs/coverage-baseline.json`.
#[derive(Debug, Clone)]
struct BaselineRelease {
    version: String,
    #[allow(dead_code)]
    timestamp: String,
    total_specs: u64,
    fully_functional: u64,
    requires_js_generators: u64,
    native_providers: u64,
    corrected_generators: u64,
    hand_audit_required: u64,
    /// The raw JSON object, preserved so we can echo the full record through
    /// the `--json` output path without reshaping fields.
    raw: serde_json::Value,
}

/// Parsed contents of `coverage-baseline.json`.
#[derive(Debug, Clone)]
struct CoverageBaseline {
    #[allow(dead_code)]
    schema_version: String,
    releases: Vec<BaselineRelease>,
}

impl CoverageBaseline {
    fn from_str(s: &str) -> Result<Self> {
        let v: serde_json::Value =
            serde_json::from_str(s).context("coverage baseline JSON is malformed")?;
        let obj = v
            .as_object()
            .context("coverage baseline JSON root must be an object")?;
        let schema_version = obj
            .get("schema_version")
            .and_then(|x| x.as_str())
            .unwrap_or("1.0")
            .to_string();
        let releases_raw = obj
            .get("releases")
            .and_then(|x| x.as_array())
            .context("coverage baseline: missing `releases` array")?;

        let mut releases = Vec::with_capacity(releases_raw.len());
        for r in releases_raw {
            let ro = r
                .as_object()
                .context("coverage baseline: release must be an object")?;
            let version = ro
                .get("version")
                .and_then(|x| x.as_str())
                .context("coverage baseline: release.version is required")?
                .to_string();
            let timestamp = ro
                .get("timestamp")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();

            let u = |key: &str| -> u64 { ro.get(key).and_then(|x| x.as_u64()).unwrap_or(0) };

            releases.push(BaselineRelease {
                version,
                timestamp,
                total_specs: u("total_specs"),
                fully_functional: u("fully_functional"),
                requires_js_generators: u("requires_js_generators"),
                native_providers: u("native_providers"),
                corrected_generators: u("corrected_generators"),
                hand_audit_required: u("hand_audit_required"),
                raw: r.clone(),
            });
        }

        Ok(Self {
            schema_version,
            releases,
        })
    }
}

/// Resolve a baseline file per task policy.
///
/// Priority order:
///   1. explicit `--baseline <path>` flag
///   2. `$GHOST_COMPLETE_BASELINE` environment variable
///   3. `docs/coverage-baseline.json` relative to the current working directory
///   4. embedded baseline shipped with the binary (`include_str!`)
fn load_baseline(explicit: Option<&Path>) -> Result<Option<CoverageBaseline>> {
    // (1) explicit path — a missing file here is an error; the user asked
    // for that specific file.
    if let Some(p) = explicit {
        if p.exists() {
            let body = std::fs::read_to_string(p)
                .with_context(|| format!("failed to read baseline {}", p.display()))?;
            return Ok(Some(CoverageBaseline::from_str(&body)?));
        } else {
            anyhow::bail!("baseline file does not exist: {}", p.display());
        }
    }

    // (2) env override. Like the explicit flag, a non-existent path is an
    // error — the user deliberately pointed us at a file, so silent
    // fall-through would mask typos. `/dev/null` is a deliberate
    // suppression knob: it exists, so this branch accepts it and the
    // parse-as-empty downstream yields a clean "malformed" error.
    if let Some(p) = std::env::var_os("GHOST_COMPLETE_BASELINE") {
        let p = PathBuf::from(p);
        if p.exists() {
            let body = std::fs::read_to_string(&p)
                .with_context(|| format!("failed to read baseline {}", p.display()))?;
            return Ok(Some(CoverageBaseline::from_str(&body)?));
        } else {
            anyhow::bail!(
                "baseline file does not exist (from GHOST_COMPLETE_BASELINE): {}",
                p.display()
            );
        }
    }

    // (3) CWD lookup.
    let cwd_path = PathBuf::from("docs/coverage-baseline.json");
    if cwd_path.exists() {
        let body = std::fs::read_to_string(&cwd_path)
            .with_context(|| format!("failed to read baseline {}", cwd_path.display()))?;
        return Ok(Some(CoverageBaseline::from_str(&body)?));
    }

    // (4) embedded fallback — only when the constant was populated at build
    // time. `include_str!` yields a compile-time string, but we still allow
    // the developer to suppress by passing `GHOST_COMPLETE_BASELINE=/dev/null`
    // (handled above: /dev/null exists but parses empty → malformed → error).
    // An empty embedded string (unlikely) counts as "no baseline".
    if EMBEDDED_BASELINE.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(CoverageBaseline::from_str(EMBEDDED_BASELINE)?))
}

/// Render the human-readable "Coverage trend" section.
///
/// Annotation semantics:
///   - `(baseline)` — only emitted in the single-row bootstrap case
///     (exactly one release in the baseline file, so there is nothing to
///     compare against yet).
///   - `(unchanged)` — emitted when there are two or more releases and
///     a specific metric is identical between prev and curr.
///   - `(+N)` / `(-N)` — signed delta when the metric actually moved.
fn render_coverage_trend(out: &mut dyn Write, baseline: Option<&CoverageBaseline>) -> Result<()> {
    writeln!(out)?;
    let baseline = match baseline {
        Some(b) if !b.releases.is_empty() => b,
        _ => {
            writeln!(out, "Coverage trend: No baseline recorded")?;
            return Ok(());
        }
    };

    // Determine prev/curr. When there's exactly one row this is the
    // bootstrap case — `is_bootstrap` switches the delta helpers from
    // `(unchanged)` to `(baseline)`.
    let is_bootstrap = baseline.releases.len() < 2;
    let (prev, curr) = if baseline.releases.len() >= 2 {
        let n = baseline.releases.len();
        (&baseline.releases[n - 2], &baseline.releases[n - 1])
    } else {
        let only = &baseline.releases[0];
        (only, only)
    };

    writeln!(out, "Coverage trend (vs previous release):")?;
    writeln!(
        out,
        "  Total specs: {} {}",
        curr.total_specs,
        delta_annotation(prev.total_specs, curr.total_specs, is_bootstrap)
    )?;
    writeln!(
        out,
        "  Fully functional: {} {}",
        curr.fully_functional,
        delta_annotation(prev.fully_functional, curr.fully_functional, is_bootstrap),
    )?;
    writeln!(
        out,
        "  Requires-JS generators: {} {}",
        pair_with_arrow(prev.requires_js_generators, curr.requires_js_generators),
        delta_annotation(
            prev.requires_js_generators,
            curr.requires_js_generators,
            is_bootstrap
        ),
    )?;
    writeln!(
        out,
        "  Native providers: {} {}",
        pair_with_arrow(prev.native_providers, curr.native_providers),
        delta_annotation(prev.native_providers, curr.native_providers, is_bootstrap),
    )?;
    writeln!(
        out,
        "  Corrected generators: {} {}",
        curr.corrected_generators,
        delta_annotation(
            prev.corrected_generators,
            curr.corrected_generators,
            is_bootstrap
        )
    )?;
    // Keep the user aware of which release row we're comparing against —
    // helps when someone runs this months after the last release.
    writeln!(out, "  (baseline: v{} → v{})", prev.version, curr.version)?;
    Ok(())
}

/// Render the delta annotation for a metric.
///
/// - `is_bootstrap = true` (single-row baseline) → `(baseline)`.
/// - Two-or-more-row baseline: `(unchanged)` when prev == curr, otherwise
///   a signed `(+N)` / `(-N)` delta.
fn delta_annotation(prev: u64, curr: u64, is_bootstrap: bool) -> String {
    if is_bootstrap {
        "(baseline)".to_string()
    } else if prev == curr {
        "(unchanged)".to_string()
    } else if curr > prev {
        format!("(+{})", curr - prev)
    } else {
        format!("(-{})", prev - curr)
    }
}

/// Render `"prev → curr"`.
fn pair_with_arrow(prev: u64, curr: u64) -> String {
    format!("{} \u{2192} {}", prev, curr)
}

/// Outcome of a `status` run — surfaces the numbers the CLI entry point
/// uses to decide the process exit code in strict mode, plus the data
/// shared between the text and JSON render paths.
#[derive(Debug, Default, Clone)]
pub struct StatusOutcome {
    pub fs_specs: usize,
    pub embedded_count: usize,
    pub total_parse_errors: usize,
    pub fully_functional: usize,
    pub partially_functional: usize,
    pub js_commands: Vec<String>,
    /// Per-dir spec-load error strings, already sanitised for terminal
    /// output. Retained so the JSON path can surface them too.
    pub parse_error_lines: Vec<String>,
}

/// Scan filesystem spec dirs and collect the numbers the status report
/// needs. Does NOT produce any output.
fn scan_specs(config_path: Option<&str>) -> Result<StatusOutcome> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;
    let dirs = resolve_spec_dirs(&config.paths.spec_dirs);
    let embedded_count = crate::install::EMBEDDED_SPECS.len();

    let mut fs_specs = 0usize;
    let mut fully_functional = 0usize;
    let mut partially_functional = 0usize;
    let mut js_commands: Vec<String> = Vec::new();
    let mut total_parse_errors = 0usize;
    let mut parse_error_lines: Vec<String> = Vec::new();

    for dir in &dirs {
        let result = SpecStore::load_from_dir(dir)?;
        let store = result.store;

        let mut specs: Vec<(&str, &CompletionSpec)> = store.iter().collect();
        specs.sort_by_key(|(name, _)| *name);

        for (name, spec) in &specs {
            fs_specs += 1;
            if has_requires_js(spec) {
                partially_functional += 1;
                js_commands.push((*name).to_string());
            } else {
                fully_functional += 1;
            }
        }

        if !result.errors.is_empty() {
            total_parse_errors += result.errors.len();
            for err in &result.errors {
                parse_error_lines.push(sanitize_for_terminal(err));
            }
        }
    }

    js_commands.sort();

    Ok(StatusOutcome {
        fs_specs,
        embedded_count,
        total_parse_errors,
        fully_functional,
        partially_functional,
        js_commands,
        parse_error_lines,
    })
}

/// Inner implementation that writes its report to `out` instead of stdout,
/// so the sanitisation path can be tested without a real terminal.
fn run_status_inner(
    config_path: Option<&str>,
    out: &mut dyn std::io::Write,
) -> Result<StatusOutcome> {
    let outcome = scan_specs(config_path)?;

    if !outcome.parse_error_lines.is_empty() {
        writeln!(
            out,
            "\x1b[33m{} spec(s) failed to load:\x1b[0m",
            outcome.parse_error_lines.len()
        )?;
        for line in &outcome.parse_error_lines {
            writeln!(out, "  \x1b[33m- {}\x1b[0m", line)?;
        }
    }

    writeln!(out, "Ghost Complete v{}\n", env!("CARGO_PKG_VERSION"))?;
    writeln!(out, "Completion specs:")?;
    writeln!(out, "  Embedded in binary:    {}", outcome.embedded_count)?;
    if outcome.fs_specs > 0 {
        writeln!(out, "  Filesystem overrides:  {}", outcome.fs_specs)?;
        writeln!(
            out,
            "  \x1b[32mFully functional:\x1b[0m      {}",
            outcome.fully_functional
        )?;
        writeln!(
            out,
            "  \x1b[33mPartially functional:\x1b[0m  {} (has requires_js generators)",
            outcome.partially_functional
        )?;
    } else {
        writeln!(
            out,
            "  Filesystem overrides:  0 (run `ghost-complete install` to deploy specs)"
        )?;
    }

    if !outcome.js_commands.is_empty() {
        writeln!(
            out,
            "\nCommands with requires_js generators ({}):",
            outcome.js_commands.len()
        )?;
        for cmd in &outcome.js_commands {
            writeln!(out, "  {}", sanitize_for_terminal(cmd))?;
        }
    }

    Ok(outcome)
}

/// Like [`run_status_inner`] but also appends the Coverage-trend section.
/// Callers that want a minimal report (e.g. tests that don't care about
/// the trend block) can still call the inner form directly.
fn run_status_inner_with_trend(
    config_path: Option<&str>,
    baseline_path: Option<&Path>,
    out: &mut dyn Write,
) -> Result<StatusOutcome> {
    let outcome = run_status_inner(config_path, out)?;
    let baseline = load_baseline(baseline_path)?;
    render_coverage_trend(out, baseline.as_ref())?;
    Ok(outcome)
}

/// Emit the JSON status report to `out`.
fn run_status_json(
    config_path: Option<&str>,
    baseline_path: Option<&Path>,
    out: &mut dyn Write,
) -> Result<StatusOutcome> {
    let outcome = scan_specs(config_path)?;
    let baseline = load_baseline(baseline_path)?;

    let coverage_trend = match baseline.as_ref() {
        None => serde_json::Value::Null,
        Some(b) if b.releases.is_empty() => serde_json::Value::Null,
        Some(b) => {
            let n = b.releases.len();
            if n == 1 {
                let curr = &b.releases[0];
                serde_json::json!({
                    "previous": serde_json::Value::Null,
                    "current": curr.raw,
                    "delta": serde_json::Value::Null,
                })
            } else {
                let prev = &b.releases[n - 2];
                let curr = &b.releases[n - 1];
                serde_json::json!({
                    "previous": prev.raw,
                    "current": curr.raw,
                    "delta": {
                        "total_specs":
                            curr.total_specs as i64 - prev.total_specs as i64,
                        "fully_functional":
                            curr.fully_functional as i64 - prev.fully_functional as i64,
                        "requires_js_generators":
                            curr.requires_js_generators as i64
                                - prev.requires_js_generators as i64,
                        "native_providers":
                            curr.native_providers as i64
                                - prev.native_providers as i64,
                        "corrected_generators":
                            curr.corrected_generators as i64
                                - prev.corrected_generators as i64,
                        "hand_audit_required":
                            curr.hand_audit_required as i64
                                - prev.hand_audit_required as i64,
                    },
                })
            }
        }
    };

    // `total` reports the canonical shipped-spec count (embedded count) —
    // filesystem specs may be overrides or additions; we do not attempt
    // to deduplicate here. Schema example in the task brief agrees.
    //
    // `parse_errors` stays as a scalar count for backwards compat;
    // `parse_error_details` mirrors the per-line sanitized messages the
    // text path emits so JSON consumers can surface them too.
    let payload = serde_json::json!({
        "schema_version": "1.0",
        "spec_counts": {
            "total": outcome.embedded_count,
            "fully_functional": outcome.fully_functional,
            "partially_functional": outcome.partially_functional,
            "embedded": outcome.embedded_count,
            "filesystem_overrides": outcome.fs_specs,
            "parse_errors": outcome.total_parse_errors,
            "parse_error_details": outcome.parse_error_lines,
        },
        "coverage_trend": coverage_trend,
    });

    let s = serde_json::to_string_pretty(&payload).context("failed to serialize status JSON")?;
    writeln!(out, "{}", s)?;
    Ok(outcome)
}

/// Render the status report. When `strict` is `true`, prints the full report
/// first and then exits with code 1 if spec health is degraded — meaning any
/// of:
///   - zero specs loaded across all configured spec dirs AND no embedded
///     specs available (nothing to complete against), or
///   - one or more spec files failed to parse (`SpecLoadResult::errors`
///     non-empty in at least one dir).
///
/// When `json` is `true`, the report is a machine-readable JSON object on
/// stdout instead of human text; strict-mode error lines are suppressed
/// (the caller reads the JSON and decides).
///
/// Non-strict, non-JSON mode preserves the prior behaviour: always returns
/// `Ok(())` regardless of spec health.
pub fn run_status_with_opts(
    config_path: Option<&str>,
    strict: bool,
    json: bool,
    baseline_path: Option<&Path>,
) -> Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    let outcome = if json {
        run_status_json(config_path, baseline_path, &mut handle)?
    } else {
        run_status_inner_with_trend(config_path, baseline_path, &mut handle)?
    };

    if strict {
        let no_specs_available = outcome.fs_specs == 0 && outcome.embedded_count == 0;
        if no_specs_available || outcome.total_parse_errors > 0 {
            if !json {
                writeln!(&mut handle)?;
                if no_specs_available {
                    writeln!(
                        &mut handle,
                        "\x1b[31mstrict mode: no specs available (0 embedded, 0 filesystem).\x1b[0m"
                    )?;
                }
                if outcome.total_parse_errors > 0 {
                    writeln!(
                        &mut handle,
                        "\x1b[31mstrict mode: {} spec file(s) failed to parse.\x1b[0m",
                        outcome.total_parse_errors
                    )?;
                }
            }
            std::process::exit(1);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a config TOML pointing at a single spec dir, write it to
    /// `tmp/config.toml`, and return its path.
    fn write_config_for(spec_dir: &std::path::Path, tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let cfg_path = tmp.path().join("config.toml");
        let body = format!(
            "[paths]\nspec_dirs = [\"{}\"]\n",
            spec_dir.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(&cfg_path, body).unwrap();
        cfg_path
    }

    /// Write a baseline JSON fixture into `tmp/coverage-baseline.json` and
    /// return its path.
    fn write_baseline(tmp: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
        let p = tmp.path().join("coverage-baseline.json");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn status_sanitizes_hostile_spec_filenames_in_errors() {
        // A hostile filename embedded in the on-disk spec dir must not
        // smuggle raw ESC bytes through `ghost-complete status` output.
        // The spec loader fails to parse this file (not valid JSON) and
        // the resulting error string embeds the filename verbatim — which
        // would otherwise reach stdout unsanitised.
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        let hostile = "\x1b[31mEVIL.json";
        std::fs::write(spec_dir.join(hostile), "not valid json").unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_status_inner(Some(cfg.to_str().unwrap()), &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);

        // The "failed to load" line is the only place user-supplied bytes
        // reach stdout in the status report. Pull it out and assert the
        // filename's raw ESC was stripped. The line still has `\x1b[33m`
        // color wrappers from our own formatter — those are fine; what
        // matters is that the *inner* error text carries no ESC.
        let err_line = txt
            .lines()
            .find(|l| l.contains("EVIL.json"))
            .unwrap_or_else(|| panic!("expected error line mentioning EVIL.json, got:\n{txt}"));
        let inner = err_line
            .trim_start_matches("  \x1b[33m- ")
            .trim_end_matches("\x1b[0m");
        assert!(
            !inner.contains('\x1b'),
            "error payload must not contain raw ESC bytes from filename, got:\n{inner:?}"
        );
        assert!(
            inner.contains("[31mEVIL.json"),
            "sanitized filename (ESC stripped, CSI params retained as literal text) \
             should appear in output:\n{inner}"
        );
    }

    // -------------------------------------------------------------------------
    // Coverage trend / baseline tests
    // -------------------------------------------------------------------------

    #[test]
    fn baseline_absent_prints_not_recorded() {
        // Cover the `load_baseline → None` codepath by exercising the
        // renderer directly. (We can't easily drive load_baseline into
        // returning Ok(None) in a test: GHOST_COMPLETE_BASELINE pointing
        // at a missing path now *errors* rather than silently falling
        // through — see `missing_env_baseline_errors` — and the embedded
        // fallback is always populated when the binary is compiled.)
        let mut out = Vec::new();
        render_coverage_trend(&mut out, None).unwrap();
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("Coverage trend: No baseline recorded"),
            "expected 'No baseline recorded' line, got:\n{txt}"
        );
    }

    #[test]
    fn baseline_single_row_prints_baseline_annotations() {
        let body = r#"{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "0.9.1",
      "timestamp": "2026-04-20T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 526,
      "requires_js_generators": 1889,
      "native_providers": 12,
      "corrected_generators": 139,
      "hand_audit_required": 866
    }
  ]
}"#;
        let tmp = tempfile::TempDir::new().unwrap();
        let p = write_baseline(&tmp, body);
        let baseline = load_baseline(Some(&p)).unwrap().unwrap();

        let mut out = Vec::new();
        render_coverage_trend(&mut out, Some(&baseline)).unwrap();
        let txt = String::from_utf8_lossy(&out);

        assert!(
            txt.contains("Coverage trend (vs previous release):"),
            "should emit header, got:\n{txt}"
        );
        // Every metric line should show (baseline) when prev == curr.
        assert!(
            txt.contains("Total specs: 709 (baseline)"),
            "Total specs line should have (baseline), got:\n{txt}"
        );
        assert!(
            txt.contains("Fully functional: 526 (baseline)"),
            "Fully functional line should have (baseline), got:\n{txt}"
        );
        assert!(
            txt.contains("Requires-JS generators: 1889 \u{2192} 1889 (baseline)"),
            "Requires-JS line should show prev→curr with (baseline), got:\n{txt}"
        );
        assert!(
            txt.contains("Native providers: 12 \u{2192} 12 (baseline)"),
            "Native providers line should show prev→curr with (baseline), got:\n{txt}"
        );
        assert!(
            txt.contains("Corrected generators: 139 (baseline)"),
            "Corrected generators line should have (baseline), got:\n{txt}"
        );
    }

    #[test]
    fn baseline_two_rows_prints_signed_deltas() {
        let body = r#"{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "0.9.1",
      "timestamp": "2026-04-20T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 526,
      "requires_js_generators": 1889,
      "native_providers": 12,
      "corrected_generators": 139,
      "hand_audit_required": 866
    },
    {
      "version": "0.10.0",
      "timestamp": "2026-05-10T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 534,
      "requires_js_generators": 1721,
      "native_providers": 20,
      "corrected_generators": 139,
      "hand_audit_required": 850
    }
  ]
}"#;
        let tmp = tempfile::TempDir::new().unwrap();
        let p = write_baseline(&tmp, body);
        let baseline = load_baseline(Some(&p)).unwrap().unwrap();

        let mut out = Vec::new();
        render_coverage_trend(&mut out, Some(&baseline)).unwrap();
        let txt = String::from_utf8_lossy(&out);

        // total_specs unchanged between two distinct rows — renders
        // (unchanged), not (baseline). (baseline) is reserved for the
        // single-row bootstrap case.
        assert!(
            txt.contains("Total specs: 709 (unchanged)"),
            "Total specs identical-across-rows should show (unchanged), got:\n{txt}"
        );
        // fully_functional: 526 → 534 (+8). Signed delta conveys the
        // change on its own — no narrative annotation.
        assert!(
            txt.contains("Fully functional: 534 (+8)"),
            "Fully functional line missing signed delta, got:\n{txt}"
        );
        assert!(
            !txt.contains("Phase 3A"),
            "Phase 3A annotation must not appear anywhere — it was removed \
             in favour of the plain signed delta, got:\n{txt}"
        );
        // requires_js_generators: 1889 → 1721 (-168)
        assert!(
            txt.contains("Requires-JS generators: 1889 \u{2192} 1721 (-168)"),
            "Requires-JS signed delta wrong, got:\n{txt}"
        );
        // native_providers: 12 → 20 (+8) — plain signed delta only.
        assert!(
            txt.contains("Native providers: 12 \u{2192} 20 (+8)"),
            "Native providers line missing signed delta, got:\n{txt}"
        );
        // Corrected identical between rows — renders (unchanged).
        assert!(
            txt.contains("Corrected generators: 139 (unchanged)"),
            "Corrected generators identical-across-rows should show (unchanged), got:\n{txt}"
        );
        // Guard: (baseline) must NOT appear anywhere in the per-metric
        // lines for a multi-row baseline (only the trailing `(baseline:
        // v…→v…)` disambiguation line is allowed to contain the word).
        let metric_lines: Vec<&str> = txt
            .lines()
            .filter(|l| {
                l.contains("Total specs:")
                    || l.contains("Fully functional:")
                    || l.contains("Requires-JS generators:")
                    || l.contains("Native providers:")
                    || l.contains("Corrected generators:")
            })
            .collect();
        for line in &metric_lines {
            assert!(
                !line.contains("(baseline)"),
                "(baseline) annotation leaked into multi-row metric line: {line}"
            );
        }
    }

    #[test]
    fn baseline_two_rows_never_emits_phase_3a_annotation() {
        // The Phase-3A narrative annotation has been removed; the signed
        // delta is the canonical signal. Guard against a regression that
        // would re-introduce the brittle value-based heuristic.
        let body = r#"{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "0.9.1",
      "timestamp": "2026-04-20T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 526,
      "requires_js_generators": 1889,
      "native_providers": 12,
      "corrected_generators": 139,
      "hand_audit_required": 866
    },
    {
      "version": "0.11.0",
      "timestamp": "2026-06-10T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 540,
      "requires_js_generators": 1700,
      "native_providers": 25,
      "corrected_generators": 139,
      "hand_audit_required": 830
    }
  ]
}"#;
        let tmp = tempfile::TempDir::new().unwrap();
        let p = write_baseline(&tmp, body);
        let baseline = load_baseline(Some(&p)).unwrap().unwrap();

        let mut out = Vec::new();
        render_coverage_trend(&mut out, Some(&baseline)).unwrap();
        let txt = String::from_utf8_lossy(&out);

        assert!(
            !txt.contains("Phase 3A"),
            "Phase 3A annotation must never appear — it was removed in \
             favour of the plain signed delta, got:\n{txt}"
        );
        assert!(
            txt.contains("Fully functional: 540 (+14)"),
            "Fully functional signed delta wrong, got:\n{txt}"
        );
    }

    #[test]
    fn baseline_two_rows_identical_metric_prints_unchanged() {
        // Two distinct releases where one metric is numerically identical
        // across both rows must render `(unchanged)` — never `(baseline)`,
        // which is reserved for the single-row bootstrap case.
        let body = r#"{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "0.9.1",
      "timestamp": "2026-04-20T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 526,
      "requires_js_generators": 1889,
      "native_providers": 12,
      "corrected_generators": 139,
      "hand_audit_required": 866
    },
    {
      "version": "0.10.0",
      "timestamp": "2026-05-10T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 530,
      "requires_js_generators": 1800,
      "native_providers": 15,
      "corrected_generators": 139,
      "hand_audit_required": 860
    }
  ]
}"#;
        let tmp = tempfile::TempDir::new().unwrap();
        let p = write_baseline(&tmp, body);
        let baseline = load_baseline(Some(&p)).unwrap().unwrap();

        let mut out = Vec::new();
        render_coverage_trend(&mut out, Some(&baseline)).unwrap();
        let txt = String::from_utf8_lossy(&out);

        // total_specs is 709 in both rows — must read (unchanged).
        assert!(
            txt.contains("Total specs: 709 (unchanged)"),
            "Total specs identical across two rows must say (unchanged), got:\n{txt}"
        );
        // corrected_generators is 139 in both rows — same.
        assert!(
            txt.contains("Corrected generators: 139 (unchanged)"),
            "Corrected generators identical across two rows must say (unchanged), got:\n{txt}"
        );
        // (baseline) must appear ONLY on the trailing disambiguation
        // line — never on a per-metric line.
        for line in txt.lines() {
            if line.starts_with("  (baseline: v") {
                continue;
            }
            assert!(
                !line.contains("(baseline)"),
                "(baseline) must not appear on any metric line in the two-row case: {line}"
            );
        }
    }

    #[test]
    fn json_flag_suppresses_text_output() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let body = r#"{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "0.9.1",
      "timestamp": "2026-04-20T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 526,
      "requires_js_generators": 1889,
      "native_providers": 12,
      "corrected_generators": 139,
      "hand_audit_required": 866
    }
  ]
}"#;
        let baseline_path = write_baseline(&tmp, body);

        let mut out = Vec::new();
        run_status_json(Some(cfg.to_str().unwrap()), Some(&baseline_path), &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);

        assert!(
            !txt.contains("Coverage trend"),
            "JSON output must not include the human-readable trend header, got:\n{txt}"
        );
        assert!(
            !txt.contains("Ghost Complete v"),
            "JSON output must not include the human-readable version banner, got:\n{txt}"
        );
        // Parses as valid JSON.
        let _parsed: serde_json::Value =
            serde_json::from_str(&txt).expect("--json output must be valid JSON");
    }

    #[test]
    fn json_flag_structure_matches_schema() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let body = r#"{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "0.9.1",
      "timestamp": "2026-04-20T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 526,
      "requires_js_generators": 1889,
      "native_providers": 12,
      "corrected_generators": 139,
      "hand_audit_required": 866
    },
    {
      "version": "0.10.0",
      "timestamp": "2026-05-10T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 534,
      "requires_js_generators": 1721,
      "native_providers": 20,
      "corrected_generators": 139,
      "hand_audit_required": 850
    }
  ]
}"#;
        let baseline_path = write_baseline(&tmp, body);

        let mut out = Vec::new();
        run_status_json(Some(cfg.to_str().unwrap()), Some(&baseline_path), &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);
        let parsed: serde_json::Value = serde_json::from_str(&txt).unwrap();

        assert_eq!(parsed["schema_version"], "1.0");
        assert!(
            parsed["spec_counts"].is_object(),
            "spec_counts must be an object"
        );
        assert!(parsed["spec_counts"]["total"].is_number());
        assert!(parsed["spec_counts"]["fully_functional"].is_number());
        assert!(parsed["spec_counts"]["partially_functional"].is_number());
        assert!(parsed["spec_counts"]["embedded"].is_number());
        assert!(parsed["spec_counts"]["filesystem_overrides"].is_number());
        assert!(parsed["spec_counts"]["parse_errors"].is_number());
        assert!(
            parsed["spec_counts"]["parse_error_details"].is_array(),
            "parse_error_details must be an array (empty when no errors)"
        );
        assert_eq!(
            parsed["spec_counts"]["parse_error_details"]
                .as_array()
                .unwrap()
                .len(),
            0,
            "no-error fixture should produce an empty parse_error_details array"
        );

        let trend = &parsed["coverage_trend"];
        assert!(trend.is_object(), "coverage_trend should be populated");
        assert_eq!(trend["previous"]["version"], "0.9.1");
        assert_eq!(trend["current"]["version"], "0.10.0");
        assert_eq!(trend["delta"]["fully_functional"], 8);
        assert_eq!(trend["delta"]["requires_js_generators"], -168);
        assert_eq!(trend["delta"]["native_providers"], 8);
        assert_eq!(trend["delta"]["total_specs"], 0);
    }

    #[test]
    fn json_flag_with_no_baseline_emits_null_trend() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        // No baseline path AND we can't easily suppress the embedded
        // fallback through a test-only env override without side-effects.
        // Instead we exercise the single-row code path with an explicit
        // empty-releases fixture, asserting delta is null.
        let body = r#"{"schema_version": "1.0", "releases": []}"#;
        let baseline_path = write_baseline(&tmp, body);

        let mut out = Vec::new();
        run_status_json(Some(cfg.to_str().unwrap()), Some(&baseline_path), &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);
        let parsed: serde_json::Value = serde_json::from_str(&txt).unwrap();
        assert!(
            parsed["coverage_trend"].is_null(),
            "empty-releases baseline should yield null trend, got: {}",
            parsed
        );
        // `parse_error_details` is present even when there are no errors:
        // empty array, not missing-key.
        assert!(
            parsed["spec_counts"]["parse_error_details"].is_array(),
            "parse_error_details must be an array in the no-baseline path too"
        );
        assert_eq!(
            parsed["spec_counts"]["parse_error_details"]
                .as_array()
                .unwrap()
                .len(),
            0,
            "no-error fixture should produce an empty parse_error_details array"
        );
    }

    #[test]
    fn malformed_baseline_json_errors_cleanly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let body = "{this is not valid json";
        let p = write_baseline(&tmp, body);

        let result = load_baseline(Some(&p));
        assert!(
            result.is_err(),
            "malformed JSON must produce Err (no panic), got: {:?}",
            result
        );
    }

    #[test]
    fn missing_explicit_baseline_errors() {
        // The user explicitly requested a baseline file — a missing file
        // is their mistake, not an invitation to fall through to the
        // embedded default.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("does-not-exist.json");
        let result = load_baseline(Some(&p));
        assert!(result.is_err());
    }

    // -------------------------------------------------------------------------
    // GHOST_COMPLETE_BASELINE env-var tests
    //
    // These tests mutate a process-wide env var. Rust's default test harness
    // runs tests concurrently within a crate, so we serialise access via a
    // crate-local mutex. `set_var` / `remove_var` are not thread-safe in the
    // presence of readers in other threads — within this small cfg(test) block
    // we ensure all touches go through `with_env_baseline`.
    // -------------------------------------------------------------------------

    static ENV_BASELINE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `body` with `GHOST_COMPLETE_BASELINE` set to `val` (or unset if
    /// `None`), restoring the previous state on return even if `body`
    /// panics. Holds the crate-local mutex so concurrent tests don't race.
    fn with_env_baseline<R>(val: Option<&std::ffi::OsStr>, body: impl FnOnce() -> R) -> R {
        let _guard = ENV_BASELINE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var_os("GHOST_COMPLETE_BASELINE");
        match val {
            Some(v) => std::env::set_var("GHOST_COMPLETE_BASELINE", v),
            None => std::env::remove_var("GHOST_COMPLETE_BASELINE"),
        }
        // Defuse Drop-based restore: use an inner closure + catch_unwind to
        // guarantee restoration even on panic.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        match prev {
            Some(p) => std::env::set_var("GHOST_COMPLETE_BASELINE", p),
            None => std::env::remove_var("GHOST_COMPLETE_BASELINE"),
        }
        match result {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e),
        }
    }

    #[test]
    fn missing_env_baseline_errors() {
        // GHOST_COMPLETE_BASELINE pointing at a non-existent path must bail
        // loudly — a silent fall-through to the embedded default would mask
        // the user's typo. Guards against a refactor that reverts the
        // env-var branch to the old silent-drop behaviour.
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist.json");

        let err = with_env_baseline(Some(missing.as_os_str()), || {
            load_baseline(None).expect_err("missing env baseline must error")
        });
        let msg = format!("{err:#}");
        assert!(
            msg.contains("GHOST_COMPLETE_BASELINE"),
            "error message should name the env var that triggered the bail, got:\n{msg}"
        );
    }

    #[test]
    fn existing_env_baseline_suppresses() {
        // An EXISTING path via GHOST_COMPLETE_BASELINE does NOT trigger the
        // missing-file bail — it is read and parsed. Documents the
        // /dev/null suppression knob the source comment promises: a file
        // that exists but parses as empty yields a clean malformed error
        // rather than a missing-file error, confirming the branch took
        // the "exists" path.
        let tmp = tempfile::TempDir::new().unwrap();
        let empty = tmp.path().join("empty.json");
        std::fs::write(&empty, "").unwrap();

        let err = with_env_baseline(Some(empty.as_os_str()), || {
            load_baseline(None).expect_err("empty JSON must parse-error")
        });
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("does not exist"),
            "an existing env-var baseline must not trip the missing-file \
             bail — parse error expected instead, got:\n{msg}"
        );
        assert!(
            msg.contains("malformed") || msg.contains("baseline"),
            "expected a parse-side error mentioning the baseline, got:\n{msg}"
        );
    }
}
