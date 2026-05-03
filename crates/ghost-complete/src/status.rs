use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gc_suggest::spec_dirs::resolve_spec_dirs;
use gc_suggest::specs::{ArgSpec, CompletionSpec, GeneratorSpec, OptionSpec, SubcommandSpec};
use gc_suggest::SpecStore;
use serde::{Deserialize, Serialize};

use crate::sanitize::sanitize_for_terminal;

/// Embedded fallback baseline. Used when no on-disk baseline is discoverable
/// (typical for a user-installed binary where the repo `docs/` directory is
/// not available). Keeps the "Coverage trend" section working out of the box.
const EMBEDDED_BASELINE: &str = include_str!("../../../docs/coverage-baseline.json");

/// Count every generator with `requires_js: true` anywhere in a parsed
/// spec tree. Used to classify a single spec as fully vs. partially
/// functional — NOT as the corpus-wide `requires_js_generators_total`.
/// The structured deserializer drops `OptionSpec.args[N>0]`, so summing
/// this over `SpecStore` undercounts the corpus by ~135 today (the
/// `scan_spec_files` raw-JSON walk is the source of truth for that
/// counter). Phase 4 will distinguish supported from unsupported
/// generators once `js_runtime` metadata lands in Phase 2.
fn count_requires_js_generators(spec: &CompletionSpec) -> usize {
    fn count_in_generators(gens: &[GeneratorSpec]) -> usize {
        gens.iter().filter(|g| g.requires_js).count()
    }
    fn count_in_arg(arg: &ArgSpec) -> usize {
        count_in_generators(&arg.generators)
    }
    fn count_in_args(args: &[ArgSpec]) -> usize {
        args.iter().map(count_in_arg).sum()
    }
    fn count_in_options(opts: &[OptionSpec]) -> usize {
        opts.iter()
            .map(|o| o.args.as_ref().map_or(0, count_in_arg))
            .sum()
    }
    fn count_in_subcommands(subs: &[SubcommandSpec]) -> usize {
        subs.iter()
            .map(|s| {
                count_in_args(&s.args)
                    + count_in_options(&s.options)
                    + count_in_subcommands(&s.subcommands)
            })
            .sum()
    }

    count_in_args(&spec.args)
        + count_in_options(&spec.options)
        + count_in_subcommands(&spec.subcommands)
}

/// Minimum sanity check that a baseline `timestamp` string resembles an RFC
/// 3339 instant. Full parsing would require pulling in a datetime crate; the
/// drift-check script (`scripts/check-coverage-baseline-drift.sh`) does the
/// definitive parse with `date -u -d`, and we just reject the obviously
/// malformed shapes here so Rust parsing doesn't silently accept gibberish.
///
/// Accepts: `YYYY-MM-DDThh:mm:ss` followed by either `Z` or `±hh:mm` / `±hhmm`,
/// with optional fractional seconds.
fn looks_like_rfc3339(s: &str) -> bool {
    let bytes = s.as_bytes();
    // Shortest valid RFC 3339 instant is 20 bytes: `YYYY-MM-DDThh:mm:ssZ`.
    if bytes.len() < 20 {
        return false;
    }
    // Positions of the fixed structural characters.
    let fixed = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    for (idx, byte) in fixed {
        if bytes[idx] != byte {
            return false;
        }
    }
    // Every other char up through second-of-minute must be a digit.
    for &i in &[0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        if !bytes[i].is_ascii_digit() {
            return false;
        }
    }
    // Tail: `Z`, or `±hh:mm`, or `±hhmm`, possibly preceded by `.fraction`.
    let last = bytes[bytes.len() - 1];
    // Either it ends in `Z`, or it ends with a numeric offset (digit after
    // `+`/`-` a few bytes earlier). We don't enforce exact offset layout —
    // the script-level check does.
    last == b'Z' || bytes.contains(&b'+') || bytes[19..].contains(&b'-')
}

/// A single release row inside `docs/coverage-baseline.json`.
///
/// `deny_unknown_fields` is intentionally omitted: baseline entries may carry
/// additional metadata fields in future schema revisions (captured in
/// `extra`) so that older ghost-complete binaries can still parse newer
/// baselines. Required fields remain load-bearing (missing → parse error).
#[derive(Debug, Clone, Deserialize, Serialize)]
struct BaselineRelease {
    version: String,
    timestamp: String,
    total_specs: u64,
    fully_functional: u64,
    requires_js_generators: u64,
    native_providers: u64,
    corrected_generators: u64,
    hand_audit_required: u64,
    /// Catch-all for fields not named above. Preserves forward compatibility
    /// with future schema additions and allows the `--json` output to echo
    /// the full record through without data loss.
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

/// Parsed contents of `coverage-baseline.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageBaseline {
    schema_version: String,
    releases: Vec<BaselineRelease>,
}

/// Supported `schema_version` for `coverage-baseline.json`. Bumped when the
/// field set changes in a way older binaries cannot understand.
const BASELINE_SCHEMA_VERSION: &str = "1.0";

impl CoverageBaseline {
    fn from_str(s: &str) -> Result<Self> {
        let parsed: Self =
            serde_json::from_str(s).context("coverage baseline JSON is malformed")?;
        anyhow::ensure!(
            parsed.schema_version == BASELINE_SCHEMA_VERSION,
            "coverage baseline: unsupported schema_version {:?} (expected {:?})",
            parsed.schema_version,
            BASELINE_SCHEMA_VERSION,
        );
        for r in &parsed.releases {
            anyhow::ensure!(
                looks_like_rfc3339(&r.timestamp),
                "coverage baseline: release.timestamp {:?} is not a valid RFC 3339 instant",
                r.timestamp,
            );
        }
        Ok(parsed)
    }
}

/// Resolve a baseline file, honouring the documented priority order.
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
    /// UX-9 counters. Most are computed from the runtime loader index
    /// (so they mirror what completion would actually see), but
    /// `requires_js_generators_total` is sourced from the file-level
    /// walk in `scan_spec_files`, NOT from `SpecStore`. The loader still
    /// undercounts requires_js generators today (`OptionSpec.args`
    /// truncates the array to its first entry, ~80 generators) so it
    /// reports below the file-level count. The file-walk count is the
    /// source of truth for that single field; Phase 6 will close the
    /// remaining gap. Phase 1 closed the name-keyed-dedupe gap by
    /// switching SpecStore to filename-stem keying.
    pub commands_addressable: usize,
    pub commands_partially_functional: usize,
    pub commands_nonfunctional: usize,
    pub requires_js_generators_total: usize,
    pub requires_js_generators_supported: usize,
    pub requires_js_generators_unsupported: usize,
    pub command_alias_conflicts: usize,
    /// File-level scan results — independent of the loader-level index so
    /// any divergence between `spec_files_total` and the loader's entry
    /// count remains visible. After Phase 1 the entry count equals
    /// `spec_files_total` (every committed file becomes an entry; no
    /// silent loss to name-keyed dedupe).
    pub file_scan: FileScan,
}

/// File-level scan, populated independently from the runtime loader index.
/// Walks the embedded specs (and on-disk override dirs) and counts both
/// files and total `requires_js: true` occurrences without going through
/// SpecStore's dedupe-by-name pipeline.
#[derive(Debug, Default, Clone, Serialize)]
pub struct FileScan {
    /// Number of `*.json` spec files seen across all embedded + override
    /// dirs, before any name-keyed deduplication.
    pub spec_files_total: usize,
    /// Total count of `requires_js: true` generators across ALL spec files
    /// (including ones that lose their addressability slot to a duplicate
    /// `name` entry). Sourced from the raw-JSON walk; the loader-level
    /// count diverges by ~135 today (~80 from `OptionSpec.args` truncation,
    /// ~55 from name-keyed dedupe), so the file walk is the source of
    /// truth for this counter until Phase 1 closes the gap.
    pub requires_js_generators_total: usize,
    /// Subset of `requires_js_generators_total` that Phase 4 can dispatch:
    /// generators with `js_runtime.kind = post_process` AND a populated
    /// `script` (or `script_template`). Sourced from the same raw-JSON walk,
    /// so this number is consistent with `requires_js_generators_total` even
    /// when `OptionSpec.args` truncation hides generators from the
    /// structured loader.
    pub requires_js_generators_supported: usize,
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
    let mut parse_error_lines: Vec<String> = Vec::new();

    let result = SpecStore::load_from_dirs(&dirs)?;
    let store = result.store;
    let total_parse_errors = result.errors.len();
    for err in &result.errors {
        parse_error_lines.push(sanitize_for_terminal(err));
    }

    let mut specs: Vec<(&str, &CompletionSpec)> = store.iter().collect();
    specs.sort_by_key(|(name, _)| *name);

    // Per-spec classification uses the structured loader: a spec is
    // partially functional iff at least one parsed generator carries
    // `requires_js: true`. This shape is what the runtime can actually
    // see at completion time. `name` here is the canonical id (filename
    // stem), so `js_commands` lists the on-shell command keys users would
    // actually type.
    for (name, spec) in &specs {
        fs_specs += 1;
        let js_count = count_requires_js_generators(spec);
        if js_count > 0 {
            partially_functional += 1;
            js_commands.push((*name).to_string());
        } else {
            fully_functional += 1;
        }
    }

    js_commands.sort();

    // File-level scan is the source of truth for `requires_js_generators_total`.
    // The structured deserializer truncates `OptionSpec.args[N>0]` (~80
    // generators today), so summing `count_requires_js_generators` over
    // SpecStore underreports against a raw JSON walk. Phase 1 closed the
    // name-keyed dedupe gap; the OptionSpec truncation gap is tracked
    // for Phase 6.
    let file_scan = scan_spec_files(&dirs)?;
    let requires_js_generators_total = file_scan.requires_js_generators_total;
    // Phase 4: classify every requires_js generator on disk into
    // supported/unsupported buckets. A generator is supported when it carries
    // `js_runtime.kind = post_process` AND ships an accompanying script (or
    // script_template) — Phase 4 dispatches that combination through the
    // QuickJS evaluator. Everything else (script_function, custom, missing
    // js_runtime metadata) stays unsupported.
    let requires_js_generators_supported = file_scan.requires_js_generators_supported;
    let requires_js_generators_unsupported =
        requires_js_generators_total.saturating_sub(requires_js_generators_supported);

    // commands_addressable: the alias index size, i.e. the number of
    // unique command keys users can type on the shell to reach a spec.
    // After Phase 1 this is ≥ entry count (709 entries + N non-conflict
    // name aliases against the embedded corpus).
    let commands_addressable = store.aliases_count();

    // command_alias_conflicts: real, runtime-visible alias collisions
    // surfaced by the loader. Each entry is one rejected alias — either
    // a duplicate `name` across files, a `name` that crashed into
    // another file's stem, or a stem that lost to a higher-precedence
    // dir.
    let command_alias_conflicts = store.conflicts().len();

    Ok(StatusOutcome {
        fs_specs,
        embedded_count,
        total_parse_errors,
        fully_functional,
        partially_functional,
        js_commands,
        parse_error_lines,
        commands_addressable,
        commands_partially_functional: partially_functional,
        // commands_nonfunctional: 0 for now. Phase 1 keeps every spec
        // addressable via its filename stem even when its `name` alias
        // is rejected; only same-stem cross-dir collisions truly hide a
        // spec, and those are deliberate user-override behavior. Phase 7
        // will surface a tighter classification (e.g. specs whose only
        // generator is requires_js and unsupported).
        commands_nonfunctional: 0,
        requires_js_generators_total,
        requires_js_generators_supported,
        requires_js_generators_unsupported,
        command_alias_conflicts,
        file_scan,
    })
}

/// Walk the same spec dirs as the runtime loader, but count individual
/// FILES, independent of SpecStore. Phase 1 keys SpecStore on filename
/// stem so the entry count now matches `spec_files_total` per directory
/// (no name-keyed dedupe), but the file-level walk is still the source
/// of truth for `requires_js_generators_total`.
///
/// Counts `requires_js: true` via a raw `serde_json::Value` walk rather
/// than going through `parse_spec_checked_and_sanitized`. The structured
/// deserializer truncates `OptionSpec.args[N>0]` because it stores
/// `Option<ArgSpec>` and `vec.into_iter().next()`s the rest away (see
/// `deserialize_option_args`, ~80 generators), so summing the loader's
/// view underreports against the on-disk corpus. Phase 6 will close
/// that gap by relaxing `OptionSpec.args` to a real `Vec<ArgSpec>`.
///
/// Errors are tolerant — a missing dir is silently skipped (matches the
/// loader's behavior). A malformed file is also skipped (its requires_js
/// count is unknowable).
fn scan_spec_files(dirs: &[PathBuf]) -> Result<FileScan> {
    let mut spec_files_total = 0usize;
    let mut requires_js_generators_total = 0usize;
    let mut requires_js_generators_supported = 0usize;

    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            spec_files_total += 1;
            let contents = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let value: serde_json::Value = match serde_json::from_str(&contents) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let counts = count_requires_js_classes_in_value(&value);
            requires_js_generators_total += counts.total;
            requires_js_generators_supported += counts.supported;
        }
    }

    Ok(FileScan {
        spec_files_total,
        requires_js_generators_total,
        requires_js_generators_supported,
    })
}

/// Result of walking a parsed spec to classify every `requires_js` generator
/// into supported/unsupported buckets. `supported` is the subset of `total`.
#[derive(Debug, Default, Clone, Copy)]
struct JsClassCounts {
    total: usize,
    supported: usize,
}

/// Walk a raw `serde_json::Value` and classify every object with
/// `"requires_js": true` according to the Phase 4 dispatch rules:
///
/// - `total` increments for every `requires_js: true` occurrence (matches
///   [`count_requires_js_in_value`]).
/// - `supported` increments only when the generator also carries
///   `js_runtime.kind == "post_process"` AND a `script` or `script_template`
///   array. That is the exact predicate
///   `gc_suggest::specs::collect_generators` uses to fall through to the
///   QuickJS dispatch path.
///
/// This is the doctor/status-side mirror of the runtime classification —
/// keeping them in sync is a load-bearing invariant for the coverage
/// counters surfaced by `status --json`.
fn count_requires_js_classes_in_value(value: &serde_json::Value) -> JsClassCounts {
    let mut stack: Vec<&serde_json::Value> = vec![value];
    let mut counts = JsClassCounts::default();
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(map) => {
                if matches!(map.get("requires_js"), Some(serde_json::Value::Bool(true))) {
                    counts.total += 1;
                    if is_post_process_supported(map) {
                        counts.supported += 1;
                    }
                }
                for v in map.values() {
                    stack.push(v);
                }
            }
            serde_json::Value::Array(arr) => {
                for v in arr {
                    stack.push(v);
                }
            }
            _ => {}
        }
    }
    counts
}

/// Returns true when a generator object has the shape Phase 4 can dispatch.
/// Mirrors `collect_generators` in `gc-suggest::specs`: the JS path needs
/// `js_runtime.kind == "post_process"` AND at least one of `script` /
/// `script_template`.
fn is_post_process_supported(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    let kind_is_post_process = map
        .get("js_runtime")
        .and_then(|v| v.as_object())
        .and_then(|rt| rt.get("kind"))
        .and_then(|k| k.as_str())
        .map(|k| k == "post_process")
        .unwrap_or(false);
    if !kind_is_post_process {
        return false;
    }
    let has_script = map
        .get("script")
        .map(|v| v.is_array() || v.is_string())
        .unwrap_or(false);
    let has_template = map
        .get("script_template")
        .map(|v| v.is_array() || v.is_string())
        .unwrap_or(false);
    has_script || has_template
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

/// Supported `schema_version` for the `ghost-complete status --json`
/// output. Bumped when the output shape changes in a backward-incompatible
/// way.
///
/// 1.0 — original shape (UX-8 era).
/// 1.1 — UX-9 Phase 0: adds `commands_addressable`,
///       `commands_(fully|partially|non)functional`,
///       `requires_js_generators_(total|supported|unsupported)`,
///       `command_alias_conflicts` to `spec_counts`, plus a top-level
///       `file_scan` block. All additions are purely additive — old
///       fields keep their meaning so existing JSON consumers still
///       parse the output unchanged.
const STATUS_SCHEMA_VERSION: &str = "1.1";

/// The shape emitted by `ghost-complete status --json`. Defining this as a
/// `#[derive(Serialize)]` struct rather than inline `json!` macros fails
/// compile if any emission site drops a field, keeping the documented
/// schema honest.
///
/// `coverage_trend` is serialized as `null` (not omitted) when there is no
/// baseline — JSON consumers depend on the key being present.
#[derive(Debug, Serialize)]
struct StatusReport {
    schema_version: &'static str,
    spec_counts: SpecCounts,
    /// File-level scan independent of the runtime loader index. Surfaced
    /// alongside `spec_counts` so consumers can distinguish a
    /// loader-deduped count from a raw file count.
    file_scan: FileScan,
    coverage_trend: Option<CoverageTrend>,
}

/// Counters surfaced under `spec_counts`. UX-9 Phase 0 adds the new
/// command_* and requires_js_generators_* fields; the legacy fields
/// (`total`, `fully_functional`, `partially_functional`, `embedded`,
/// `filesystem_overrides`, `parse_errors`, `parse_error_details`) keep
/// their meaning so 1.0 consumers keep working unchanged.
#[derive(Debug, Serialize)]
struct SpecCounts {
    total: usize,
    fully_functional: usize,
    partially_functional: usize,
    embedded: usize,
    filesystem_overrides: usize,
    parse_errors: usize,
    parse_error_details: Vec<String>,
    /// Total resolvable command aliases — every key the loader would
    /// match on the shell. Counts filename stems (canonical ids) plus
    /// non-conflicting `CompletionSpec.name` aliases. Always ≥
    /// `commands_fully_functional + commands_partially_functional`
    /// because a single spec can register multiple aliases.
    commands_addressable: usize,
    /// Aliased to `fully_functional` until Phase 4 changes the definition
    /// (a partially-functional command with all its requires_js generators
    /// successfully activated will be promoted to fully functional). For
    /// Phase 0 the two fields are numerically identical.
    commands_fully_functional: usize,
    commands_partially_functional: usize,
    /// Today: 0. Phase 1 keeps every spec addressable via its filename
    /// stem even when its `name` alias is rejected, so no spec is
    /// silently lost from a single configured dir. Phase 7 will lift
    /// this once we classify specs whose only completion path is a
    /// requires_js generator that the runtime can't execute yet.
    commands_nonfunctional: usize,
    /// Total `requires_js: true` generator instances across the corpus
    /// (counted per occurrence, not per spec). Sourced from a raw
    /// `serde_json::Value` walk — equivalent to
    /// `[.. | objects | select(.requires_js == true)] | length` —
    /// because the structured loader silently drops some generator
    /// slots (see `scan_spec_files`). Equal to
    /// `file_scan.requires_js_generators_total`.
    requires_js_generators_total: usize,
    /// Today: 0. Phase 4 lifts this as post-process generators activate.
    requires_js_generators_supported: usize,
    /// `requires_js_generators_total - requires_js_generators_supported`.
    /// Surfaced as its own field so consumers don't need to subtract.
    requires_js_generators_unsupported: usize,
    /// Runtime alias collisions surfaced by the loader. After Phase 1
    /// SpecStore keys on filename stem (canonical id) plus the spec's
    /// `name` field as a secondary alias when free; an entry here means
    /// either a `name` alias was rejected because another file already
    /// owned that key (DuplicateName / NameMatchesOtherStem) or a stem
    /// lost to a higher-precedence source dir (DirectoryPrecedence).
    /// Each conflict carries source-dir + alias diagnostics in
    /// `SpecStore::conflicts()`. A user-facing per-conflict breakdown
    /// in `ghost-complete doctor` will land in Phase 7; today this
    /// count is the only signal users see for the conflict set.
    command_alias_conflicts: usize,
}

#[derive(Debug, Serialize)]
struct CoverageTrend {
    /// `null` on the bootstrap (single-row) case.
    previous: Option<BaselineRelease>,
    current: BaselineRelease,
    /// `null` on the bootstrap (single-row) case.
    delta: Option<CoverageDelta>,
}

#[derive(Debug, Serialize)]
struct CoverageDelta {
    total_specs: i64,
    fully_functional: i64,
    requires_js_generators: i64,
    native_providers: i64,
    corrected_generators: i64,
    hand_audit_required: i64,
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
        None => None,
        Some(b) if b.releases.is_empty() => None,
        Some(b) => {
            let n = b.releases.len();
            if n == 1 {
                let curr = &b.releases[0];
                Some(CoverageTrend {
                    previous: None,
                    current: curr.clone(),
                    delta: None,
                })
            } else {
                let prev = &b.releases[n - 2];
                let curr = &b.releases[n - 1];
                Some(CoverageTrend {
                    previous: Some(prev.clone()),
                    current: curr.clone(),
                    delta: Some(CoverageDelta {
                        total_specs: curr.total_specs as i64 - prev.total_specs as i64,
                        fully_functional: curr.fully_functional as i64
                            - prev.fully_functional as i64,
                        requires_js_generators: curr.requires_js_generators as i64
                            - prev.requires_js_generators as i64,
                        native_providers: curr.native_providers as i64
                            - prev.native_providers as i64,
                        corrected_generators: curr.corrected_generators as i64
                            - prev.corrected_generators as i64,
                        hand_audit_required: curr.hand_audit_required as i64
                            - prev.hand_audit_required as i64,
                    }),
                })
            }
        }
    };

    // `total` reports the canonical shipped-spec count (embedded count).
    // `filesystem_overrides` is the count of filesystem specs after
    // first-match-wins deduplication across configured spec_dirs — earlier
    // directories win over later ones (see SpecStore::load_from_dirs).
    //
    // `parse_errors` stays as a scalar count for backwards compat;
    // `parse_error_details` mirrors the per-line sanitized messages the
    // text path emits so JSON consumers can surface them too.
    let payload = StatusReport {
        schema_version: STATUS_SCHEMA_VERSION,
        spec_counts: SpecCounts {
            total: outcome.embedded_count,
            fully_functional: outcome.fully_functional,
            partially_functional: outcome.partially_functional,
            embedded: outcome.embedded_count,
            filesystem_overrides: outcome.fs_specs,
            parse_errors: outcome.total_parse_errors,
            parse_error_details: outcome.parse_error_lines.clone(),
            commands_addressable: outcome.commands_addressable,
            commands_fully_functional: outcome.fully_functional,
            commands_partially_functional: outcome.commands_partially_functional,
            commands_nonfunctional: outcome.commands_nonfunctional,
            requires_js_generators_total: outcome.requires_js_generators_total,
            requires_js_generators_supported: outcome.requires_js_generators_supported,
            requires_js_generators_unsupported: outcome.requires_js_generators_unsupported,
            command_alias_conflicts: outcome.command_alias_conflicts,
        },
        file_scan: outcome.file_scan.clone(),
        coverage_trend,
    };

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
        write_config_for_dirs(&[spec_dir], tmp)
    }

    /// Build a config TOML pointing at multiple spec dirs in order, write it to
    /// `tmp/config.toml`, and return its path.
    fn write_config_for_dirs(
        spec_dirs: &[&std::path::Path],
        tmp: &tempfile::TempDir,
    ) -> std::path::PathBuf {
        let cfg_path = tmp.path().join("config.toml");
        let dirs = spec_dirs
            .iter()
            .map(|dir| format!("\"{}\"", dir.display().to_string().replace('\\', "\\\\")))
            .collect::<Vec<_>>()
            .join(", ");
        let body = format!("[paths]\nspec_dirs = [{}]\n", dirs);
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

    #[test]
    fn status_counts_first_match_wins_store_across_resolved_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary_dir = tmp.path().join("primary-specs");
        let fallback_dir = tmp.path().join("fallback-specs");
        std::fs::create_dir_all(&primary_dir).unwrap();
        std::fs::create_dir_all(&fallback_dir).unwrap();

        std::fs::write(
            primary_dir.join("dup.json"),
            r#"{
                "name": "dup",
                "args": [{"name": "target"}]
            }"#,
        )
        .unwrap();
        std::fs::write(
            fallback_dir.join("dup.json"),
            r#"{
                "name": "dup",
                "args": [{
                    "name": "target",
                    "generators": [{"script": ["echo", "x"], "requires_js": true}]
                }]
            }"#,
        )
        .unwrap();
        std::fs::write(
            fallback_dir.join("only-fallback.json"),
            r#"{
                "name": "only-fallback",
                "args": [{
                    "name": "target",
                    "generators": [{"script": ["echo", "x"], "requires_js": true}]
                }]
            }"#,
        )
        .unwrap();

        let cfg = write_config_for_dirs(&[&primary_dir, &fallback_dir], &tmp);
        let outcome = scan_specs(Some(cfg.to_str().unwrap())).unwrap();

        assert_eq!(outcome.fs_specs, 2);
        assert_eq!(outcome.fully_functional, 1);
        assert_eq!(outcome.partially_functional, 1);
        assert_eq!(outcome.js_commands, vec!["only-fallback"]);
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

        assert_eq!(parsed["schema_version"], "1.1");
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
        // UX-9 Phase 0 additions — schema 1.1.
        assert!(parsed["spec_counts"]["commands_addressable"].is_number());
        assert!(parsed["spec_counts"]["commands_fully_functional"].is_number());
        assert!(parsed["spec_counts"]["commands_partially_functional"].is_number());
        assert!(parsed["spec_counts"]["commands_nonfunctional"].is_number());
        assert!(parsed["spec_counts"]["requires_js_generators_total"].is_number());
        assert!(parsed["spec_counts"]["requires_js_generators_supported"].is_number());
        assert!(parsed["spec_counts"]["requires_js_generators_unsupported"].is_number());
        assert!(parsed["spec_counts"]["command_alias_conflicts"].is_number());
        assert!(
            parsed["file_scan"].is_object(),
            "file_scan top-level block must be present in 1.1"
        );
        assert!(parsed["file_scan"]["spec_files_total"].is_number());
        assert!(parsed["file_scan"]["requires_js_generators_total"].is_number());
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

    /// UX-9 Phase 0: `status --json` schema 1.1 must expose every new
    /// counter field. This test exercises the wiring without depending on
    /// the corpus content (uses two synthetic fixtures so the numbers are
    /// pinned).
    #[test]
    fn status_json_exposes_new_counters() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        // One static-only spec — should land in fully_functional.
        std::fs::write(
            spec_dir.join("static-cmd.json"),
            r#"{
                "name": "static-cmd",
                "subcommands": [{"name": "go"}]
            }"#,
        )
        .unwrap();
        // One spec with a single requires_js generator — should land in
        // partially_functional and contribute 1 to the requires_js totals.
        std::fs::write(
            spec_dir.join("partial-cmd.json"),
            r#"{
                "name": "partial-cmd",
                "args": [{
                    "name": "thing",
                    "generators": [{"requires_js": true, "js_source": "ctx => []"}]
                }]
            }"#,
        )
        .unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_status_json(Some(cfg.to_str().unwrap()), None, &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);
        let parsed: serde_json::Value = serde_json::from_str(&txt).unwrap();

        // Schema 1.1 surfaces every new counter as a numeric value.
        assert_eq!(parsed["schema_version"], "1.1");
        let counts = &parsed["spec_counts"];
        assert_eq!(
            counts["commands_addressable"].as_u64().unwrap(),
            2,
            "two specs in dir → two addressable commands"
        );
        assert_eq!(counts["commands_fully_functional"].as_u64().unwrap(), 1);
        assert_eq!(counts["commands_partially_functional"].as_u64().unwrap(), 1);
        assert_eq!(counts["commands_nonfunctional"].as_u64().unwrap(), 0);
        assert_eq!(
            counts["requires_js_generators_total"].as_u64().unwrap(),
            1,
            "one requires_js generator across both fixtures"
        );
        assert_eq!(
            counts["requires_js_generators_supported"].as_u64().unwrap(),
            0,
            "Phase 0 supports zero requires_js generators"
        );
        assert_eq!(
            counts["requires_js_generators_unsupported"]
                .as_u64()
                .unwrap(),
            1
        );
        assert_eq!(
            counts["command_alias_conflicts"].as_u64().unwrap(),
            0,
            "no duplicate names → no alias conflicts"
        );

        // file_scan is a new top-level block.
        let fs_block = &parsed["file_scan"];
        assert_eq!(fs_block["spec_files_total"].as_u64().unwrap(), 2);
        assert_eq!(
            fs_block["requires_js_generators_total"].as_u64().unwrap(),
            1
        );
    }

    /// UX-9 Phase 0: `commands_partially_functional` is the count of specs
    /// where at least one generator carries `requires_js: true`. The runtime
    /// loader is the source of truth — not a raw jq scan — so this test
    /// covers the wiring through `scan_specs`.
    #[test]
    fn status_partial_count_matches_runtime_loader() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(
            spec_dir.join("static-cmd.json"),
            r#"{"name": "static-cmd", "subcommands": [{"name": "go"}]}"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("partial-cmd.json"),
            r#"{
                "name": "partial-cmd",
                "args": [{
                    "name": "thing",
                    "generators": [{"requires_js": true}]
                }]
            }"#,
        )
        .unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let outcome = scan_specs(Some(cfg.to_str().unwrap())).unwrap();
        assert_eq!(outcome.fs_specs, 2);
        assert_eq!(outcome.fully_functional, 1);
        assert_eq!(outcome.partially_functional, 1);
        assert_eq!(outcome.commands_addressable, 2);
        assert_eq!(outcome.commands_partially_functional, 1);
        assert_eq!(outcome.requires_js_generators_total, 1);
        assert_eq!(outcome.requires_js_generators_unsupported, 1);
        assert_eq!(outcome.requires_js_generators_supported, 0);
        // file-level scan agrees with loader-level scan when there are no
        // alias conflicts.
        assert_eq!(outcome.file_scan.spec_files_total, 2);
        assert_eq!(outcome.file_scan.requires_js_generators_total, 1);
        assert_eq!(outcome.command_alias_conflicts, 0);
    }

    /// UX-9 Phase 0 fixtures parse cleanly and produce the expected counter
    /// classifications. Guards the on-disk fixture files in
    /// `crates/gc-suggest/tests/fixtures/ux9/` against bit-rot.
    #[test]
    fn ux9_active_fixtures_classify_correctly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();

        // Copy the active (non-parked) fixtures into the spec dir under
        // their on-disk filenames. Skip the README + the parked subdir.
        let fixtures_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../gc-suggest/tests/fixtures/ux9");
        for entry in std::fs::read_dir(&fixtures_root)
            .expect("ux9 fixtures dir must exist alongside ghost-complete")
        {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let dest = spec_dir.join(path.file_name().unwrap());
            std::fs::copy(&path, &dest).unwrap();
        }

        let cfg = write_config_for(&spec_dir, &tmp);
        let outcome = scan_specs(Some(cfg.to_str().unwrap())).unwrap();

        // The active fixture set, after Phase 1 stem-keying and Phase 2
        // activating the two formerly-parked js_runtime fixtures:
        //   static_only.json                    → stem `static_only`, name `static-only`,
        //                                          fully functional, 0 requires_js
        //   partial_unsupported_js.json         → stem `partial_unsupported_js`,
        //                                          name `partial-unsupported-js`,
        //                                          partially functional, 1 requires_js
        //   name_mismatch.json                  → stem `name_mismatch`, name
        //                                          `alias-target`, fully functional
        //   duplicate_name_a.json + b.json      → stems `duplicate_name_a` and
        //                                          `duplicate_name_b`. Both declare
        //                                          `name: "duplicate"`. The
        //                                          alphabetically-first file (`a`)
        //                                          wins the `duplicate` alias; the
        //                                          second surfaces a DuplicateName
        //                                          conflict but stays addressable
        //                                          via its stem.
        //   post_process_supported.json (Phase 2) → stem `post_process_supported`,
        //                                          name `post-process-supported`,
        //                                          partially functional, 1 requires_js
        //                                          (js_runtime.kind = post_process;
        //                                          Phase 4 wires runtime evaluation,
        //                                          today still skipped).
        //   custom_unsupported.json (Phase 2)   → stem `custom_unsupported`,
        //                                          name `custom-unsupported`,
        //                                          partially functional, 1 requires_js
        //                                          (js_runtime.kind = custom;
        //                                          Phase 5 wires runtime evaluation,
        //                                          today still skipped).
        //
        // file_scan sees all 7 files; SpecStore keeps all 7 entries (filename
        // stems unique). commands_addressable counts the 7 stems plus 6
        // non-conflicting name aliases (`static-only`, `partial-unsupported-js`,
        // `alias-target`, `duplicate`, `post-process-supported`,
        // `custom-unsupported`).
        assert_eq!(outcome.file_scan.spec_files_total, 7);
        assert_eq!(
            outcome.fs_specs, 7,
            "Phase 1: every committed file is a unique entry"
        );
        assert_eq!(
            outcome.commands_addressable, 13,
            "7 stems + 6 non-conflicting name aliases (one duplicate name rejected)"
        );
        assert_eq!(
            outcome.command_alias_conflicts, 1,
            "duplicate_name_b loses the `duplicate` alias to duplicate_name_a"
        );
        assert_eq!(outcome.partially_functional, 3);
        assert_eq!(outcome.fully_functional, 4);
        assert_eq!(outcome.requires_js_generators_total, 3);
        // Phase 4 wires `post_process_supported` (kind = post_process WITH a
        // populated `script`) into the QuickJS dispatch path. The other two
        // requires_js fixtures stay unsupported: `custom_unsupported` is a
        // Phase 5 (`Custom`) shape, and `partial_unsupported_js` predates
        // `js_runtime` entirely.
        assert_eq!(outcome.requires_js_generators_supported, 1);
        assert_eq!(outcome.requires_js_generators_unsupported, 2);

        // js_commands lists the canonical id (filename stem) of every
        // partially-functional spec, in alphabetical order.
        assert_eq!(
            outcome.js_commands,
            vec![
                "custom_unsupported",
                "partial_unsupported_js",
                "post_process_supported",
            ]
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
    fn unsupported_schema_version_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let body = r#"{"schema_version":"2.0","releases":[]}"#;
        let p = write_baseline(&tmp, body);

        let err = load_baseline(Some(&p)).expect_err("unsupported schema_version must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("schema_version"),
            "error must name schema_version, got:\n{msg}"
        );
        assert!(
            msg.contains("1.0"),
            "error must name the expected version 1.0, got:\n{msg}"
        );
    }

    #[test]
    fn unknown_top_level_field_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let body = r#"{"schema_version":"1.0","releases":[],"surprise":1}"#;
        let p = write_baseline(&tmp, body);

        let result = load_baseline(Some(&p));
        assert!(
            result.is_err(),
            "unknown top-level field must error (deny_unknown_fields), got: {:?}",
            result
        );
    }

    #[test]
    fn malformed_timestamp_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let body = r#"{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "0.9.1",
      "timestamp": "not-a-date",
      "total_specs": 709,
      "fully_functional": 526,
      "requires_js_generators": 1889,
      "native_providers": 12,
      "corrected_generators": 139,
      "hand_audit_required": 866
    }
  ]
}"#;
        let p = write_baseline(&tmp, body);

        let err = load_baseline(Some(&p)).expect_err("garbage timestamp must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("RFC 3339"),
            "error must mention RFC 3339, got:\n{msg}"
        );
    }

    #[test]
    fn looks_like_rfc3339_branch_coverage() {
        assert!(looks_like_rfc3339("2026-04-24T00:00:00Z"));
        assert!(looks_like_rfc3339("2026-04-24T00:00:00.123+02:00"));
        assert!(looks_like_rfc3339("2026-04-24T00:00:00-04:00"));

        assert!(!looks_like_rfc3339(""));
        assert!(!looks_like_rfc3339("2026-04-24"));
        assert!(!looks_like_rfc3339("2026/04/24T00:00:00Z"));
        assert!(!looks_like_rfc3339("2026-04-24T00:00:00"));
        assert!(!looks_like_rfc3339("XXXX-04-24T00:00:00Z"));
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
