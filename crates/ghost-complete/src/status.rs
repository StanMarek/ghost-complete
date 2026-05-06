use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use gc_suggest::spec_dirs::{partition_spec_dirs, resolve_spec_dirs};
use gc_suggest::specs::{
    AliasConflict, AliasConflictDisposition, AliasConflictKind, ArgSpec, CompletionSpec,
    GeneratorSpec, OptionSpec, SpecSource, SubcommandSpec,
};
use gc_suggest::{SpecLocation, SpecStore};
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
/// this over `SpecStore` undercounts the corpus today (the `scan_spec_files`
/// raw-JSON walk is the source of truth for that counter). Supported vs.
/// unsupported generator totals are also derived from the raw JSON scan.
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
    /// Directory scan and per-spec lazy parse error strings, already
    /// sanitised for terminal output. Retained so the JSON path can surface
    /// them too.
    pub parse_error_lines: Vec<String>,
    /// Most counters mirror the runtime loader index (so they reflect what
    /// completion actually sees), but `requires_js_generators_total` is
    /// sourced from `scan_spec_files`'s raw-JSON walk, NOT from
    /// `SpecStore`. The structured loader keeps the first option arg in
    /// `OptionSpec.args` and the rest in `extra_args`, so naive sums over
    /// `args` would underreport against the source corpus; the file-walk
    /// count is the source of truth for that single field.
    pub commands_addressable: usize,
    pub commands_partially_functional: usize,
    pub commands_nonfunctional: usize,
    pub requires_js_generators_total: usize,
    pub requires_js_generators_supported: usize,
    pub requires_js_generators_unsupported: usize,
    pub command_alias_conflicts: usize,
    /// Per-conflict details surfaced from `SpecStore::conflicts()`. Drives
    /// the structured alias-conflict list in both text and JSON output so
    /// users can distinguish rejected aliases from lazy fallback candidates.
    pub command_alias_conflict_details: Vec<AliasConflictRecord>,
    /// JS runtime kill switch state (`suggest.providers.js_runtime`).
    /// Surfaced so users can see at a glance whether their requires_js
    /// generators will run.
    pub js_runtime_enabled: bool,
    /// File-level scan results — sourced from the JSON behind each resolved
    /// runtime `SpecEntry` so the raw generator counters follow completion
    /// lookup fallback behavior.
    pub file_scan: FileScan,
}

/// Serialised view of a single [`AliasConflict`]. Distinct from the
/// `gc_suggest::specs::AliasConflict` type because that one is not
/// `Serialize` (it owns owner/disposition details that aren't part of a stable
/// JSON contract). This struct carries only the fields the JSON consumer
/// needs to classify and explain the alias chain.
#[derive(Debug, Clone, Serialize)]
pub struct AliasConflictRecord {
    /// The contended alias.
    pub alias: String,
    /// Conflict kind as a snake_case string for stable JSON output.
    pub kind: String,
    /// Loader disposition for the lower-precedence candidate.
    pub disposition: String,
    /// Filename stem of the primary spec for the alias.
    pub winner_stem: String,
    /// Source dir of the primary owner.
    pub winner_dir: String,
    /// `CompletionSpec.name` declared by the primary owner. Useful for
    /// surfacing the actual conflict target (e.g. primary stem `kubectl`
    /// declared `name: kubectl` while lower-precedence `kubecolor.json`
    /// also declared `name: kubectl`).
    pub winner_name: String,
    /// Filename stem of the lower-precedence spec.
    pub loser_stem: String,
    /// Source dir of the lower-precedence spec.
    pub loser_dir: String,
    /// `CompletionSpec.name` declared by the lower-precedence spec.
    pub loser_name: String,
}

impl AliasConflictRecord {
    fn from_conflict(c: &AliasConflict) -> Self {
        Self {
            alias: c.alias.clone(),
            kind: alias_conflict_kind_str(&c.kind).to_string(),
            disposition: alias_conflict_disposition_str(&c.disposition).to_string(),
            winner_stem: c.winner.filename_stem.clone(),
            winner_dir: c.winner.source_dir.display().to_string(),
            winner_name: c.winner.spec_name.clone(),
            loser_stem: c.loser.filename_stem.clone(),
            loser_dir: c.loser.source_dir.display().to_string(),
            loser_name: c.loser.spec_name.clone(),
        }
    }
}

/// Stable snake_case rendering for `AliasConflictDisposition`.
fn alias_conflict_disposition_str(disposition: &AliasConflictDisposition) -> &'static str {
    match disposition {
        AliasConflictDisposition::Rejected => "rejected",
        AliasConflictDisposition::FallbackCandidate => "fallback_candidate",
    }
}

/// Stable snake_case rendering for `AliasConflictKind`. Keeps the JSON
/// schema decoupled from the Rust enum's `Debug` representation so
/// renaming a variant doesn't accidentally break consumers.
fn alias_conflict_kind_str(kind: &AliasConflictKind) -> &'static str {
    match kind {
        AliasConflictKind::DuplicateName => "duplicate_name",
        AliasConflictKind::NameMatchesOtherStem => "name_matches_other_stem",
        AliasConflictKind::DirectoryPrecedence => "directory_precedence",
    }
}

/// File-level scan, populated from the runtime loader's kept entries.
/// Walks both embedded and filesystem sources and counts total
/// `requires_js: true` occurrences from raw JSON without relying on the
/// structured `CompletionSpec` shape.
#[derive(Debug, Default, Clone, Serialize)]
pub struct FileScan {
    /// Number of resolved runtime spec JSON sources, after lazy fallback
    /// candidates have been reduced to the first successful source.
    pub spec_files_total: usize,
    /// Total count of `requires_js: true` generators across resolved
    /// runtime spec JSON sources after lazy fallback resolution. Sourced
    /// from the raw-JSON walk because the structured loader stores
    /// trailing option args in `extra_args`, so a naive sum over
    /// `OptionSpec.args` would underreport.
    pub requires_js_generators_total: usize,
    /// Subset of `requires_js_generators_total` that the engine can
    /// dispatch — generators carrying any of the three supported
    /// `js_runtime.kind` shapes (post_process+script, script_function,
    /// custom). Sourced from the same raw-JSON walk so this number stays
    /// consistent with `requires_js_generators_total`.
    pub requires_js_generators_supported: usize,
    /// Class breakdown of `requires_js_generators_supported`. The three
    /// per-kind fields sum to `requires_js_generators_supported` and are
    /// surfaced in JSON as `requires_js_generators_supported_by_kind`.
    pub requires_js_generators_supported_post_process: usize,
    pub requires_js_generators_supported_script_function: usize,
    pub requires_js_generators_supported_custom: usize,
}

/// True when status should supplement resolved filesystem dirs with the
/// embedded corpus. A valid explicit `paths.spec_dirs` is an exact override;
/// otherwise the runtime falls back to embedded specs after auto-detection.
fn include_embedded_for_configured_dirs(configured: &[String]) -> bool {
    if configured.is_empty() {
        return true;
    }

    partition_spec_dirs(configured).valid.is_empty()
}

/// Scan resolved runtime specs and collect the numbers the status report
/// needs. Does NOT produce any output.
fn scan_specs(config_path: Option<&str>) -> Result<StatusOutcome> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;
    let dirs = resolve_spec_dirs(&config.paths.spec_dirs);
    let include_embedded = include_embedded_for_configured_dirs(&config.paths.spec_dirs);

    scan_resolved_specs(&config, &dirs, include_embedded)
}

fn scan_resolved_specs(
    config: &gc_config::GhostConfig,
    dirs: &[PathBuf],
    include_embedded: bool,
) -> Result<StatusOutcome> {
    let embedded_count = crate::install::EMBEDDED_SPECS.len();

    let mut fs_specs = 0usize;
    let mut fully_functional = 0usize;
    let mut partially_functional = 0usize;
    let mut js_commands: Vec<String> = Vec::new();
    let mut parse_error_lines: Vec<String> = Vec::new();

    let result = if include_embedded {
        SpecStore::load_with_embedded(dirs)?
    } else {
        SpecStore::load_from_dirs(dirs)?
    };
    let store = result.store;
    for err in &result.directory_errors {
        parse_error_lines.push(sanitize_for_terminal(err));
    }

    // resolved_entries() force-loads every registered candidate, so any
    // per-entry lazy-parse failures are pinned in the parse slot by the time
    // we call force_load_errors below.
    let mut specs: Vec<(&str, Arc<CompletionSpec>, bool)> = Vec::new();
    for entry in store.resolved_entries() {
        let is_filesystem = matches!(&entry.source, SpecSource::Filesystem(_));
        if let Some(spec) = entry.spec() {
            specs.push((entry.id.as_str(), spec, is_filesystem));
        }
    }
    specs.sort_by_key(|(name, _, _)| *name);

    // Surface lazy-parse failures alongside directory-level errors.
    // The per-entry path carries its real source so operators can locate
    // the offending file even when duplicate stems exist across spec dirs.
    for err in store.force_load_errors() {
        let label = match &err.source {
            SpecLocation::Filesystem { path, .. } => {
                format!("{}: {}", path.display(), err.error)
            }
            SpecLocation::Embedded { stem } => {
                format!("<embedded>/{}.json: {}", stem, err.error)
            }
        };
        parse_error_lines.push(sanitize_for_terminal(&label));
    }
    let total_parse_errors = parse_error_lines.len();

    // Per-spec classification uses the structured loader: a spec is
    // partially functional iff at least one parsed generator carries
    // `requires_js: true`. This shape is what the runtime can actually
    // see at completion time. `name` here is the canonical id (filename
    // stem), so `js_commands` lists the on-shell command keys users would
    // actually type.
    for (name, spec, is_filesystem) in specs {
        if is_filesystem {
            fs_specs += 1;
        }
        let js_count = count_requires_js_generators(spec.as_ref());
        if js_count > 0 {
            partially_functional += 1;
            js_commands.push(name.to_string());
        } else {
            fully_functional += 1;
        }
    }

    js_commands.sort();

    // File-level scan is the source of truth for
    // `requires_js_generators_total`. The structured loader stores
    // trailing option args in `extra_args`, so summing
    // `count_requires_js_generators` over SpecStore underreports against a
    // raw JSON walk.
    //
    // Walk the SpecStore's resolved entries instead of read_dir-ing every
    // configured spec_dir: when two sources ship a copy of the same spec
    // (e.g. user-config + embedded), lazy fallback keeps the hidden
    // candidates registered, but runtime resolution selects only one source.
    let file_scan = scan_spec_files(&store)?;
    let requires_js_generators_total = file_scan.requires_js_generators_total;
    // Classify every requires_js generator on disk into supported /
    // unsupported buckets. `post_process` requires non-empty source plus
    // an accompanying script/script_template; `script_function` and
    // `custom` require non-empty source.
    let requires_js_generators_supported = file_scan.requires_js_generators_supported;
    let requires_js_generators_unsupported =
        requires_js_generators_total.saturating_sub(requires_js_generators_supported);

    // commands_addressable: the alias index size, i.e. the number of
    // unique command keys users can type on the shell. Hidden fallback
    // candidates share aliases with higher-precedence candidates, so this is
    // a command-key count rather than a raw registered-entry count.
    let commands_addressable = store.aliases_count();
    let commands_nonfunctional = store.nonfunctional_aliases_count();

    // command_alias_conflicts: real, runtime-visible alias collisions surfaced
    // by the loader. Each entry is either a rejected alias or a
    // lower-precedence fallback candidate behind a primary alias owner.
    let conflicts = store.conflicts();
    let command_alias_conflicts = conflicts.len();
    let command_alias_conflict_details = conflicts
        .iter()
        .map(AliasConflictRecord::from_conflict)
        .collect();

    let js_runtime_enabled = config.suggest.providers.js_runtime;

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
        commands_nonfunctional,
        requires_js_generators_total,
        requires_js_generators_supported,
        requires_js_generators_unsupported,
        command_alias_conflicts,
        command_alias_conflict_details,
        js_runtime_enabled,
        file_scan,
    })
}

/// Walk the resolved spec entries the runtime loader will actually use and
/// count `requires_js: true` generators across them. Hidden fallback
/// candidates remain registered for lazy failover, so `spec_files_total`
/// reflects resolved runtime sources, not raw registration entries.
///
/// Counts `requires_js: true` via a raw `serde_json::Value` walk rather
/// than going through `parse_spec_checked_and_sanitized`. The structured
/// deserializer keeps the first option arg in `OptionSpec.args` and the
/// rest in `extra_args` (see `deserialize_option_args`); a sum that only
/// reads `args` would underreport against the source corpus, so the raw
/// JSON walk is the source of truth for this counter.
///
/// Iterates [`SpecStore::resolved_entries`] and reads each [`SpecSource`]
/// directly so two overlapping sources shipping copies of the same filename
/// do NOT cause every fallback candidate's requires_js generators to be
/// counted. The scan mirrors runtime resolution instead of re-walking every
/// configured directory or the full embedded corpus in isolation.
///
/// Errors are tolerant — a missing path is silently skipped (matches
/// the loader's behavior). Read or parse failures emit a `tracing::warn!`
/// with the file path so the operator at least sees the skip, and the
/// affected file is NOT counted toward `spec_files_total` (incrementing
/// the file count but skipping its requires_js totals would produce
/// silently inconsistent counters — supported and unsupported wouldn't
/// sum back to a per-file source-of-truth).
fn scan_spec_files(store: &SpecStore) -> Result<FileScan> {
    let mut scan = FileScan::default();

    for entry in store.resolved_entries() {
        let (contents, source_label): (std::borrow::Cow<'_, str>, String) = match &entry.source {
            SpecSource::Filesystem(path) => {
                if !path.exists() {
                    continue;
                }
                let contents = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            file = %path.display(),
                            error = %e,
                            "status file scan: skipping spec (read failed); requires_js totals undercount"
                        );
                        continue;
                    }
                };
                (
                    std::borrow::Cow::Owned(contents),
                    path.display().to_string(),
                )
            }
            SpecSource::Embedded(contents) => (
                std::borrow::Cow::Borrowed(*contents),
                format!("<embedded>/{}.json", entry.filename_stem),
            ),
        };
        let value: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    file = %source_label,
                    error = %e,
                    "status file scan: skipping spec (parse failed); requires_js totals undercount"
                );
                continue;
            }
        };
        scan.spec_files_total += 1;
        let counts = count_requires_js_classes_in_value(&value);
        scan.requires_js_generators_total += counts.total;
        scan.requires_js_generators_supported += counts.supported;
        scan.requires_js_generators_supported_post_process += counts.post_process;
        scan.requires_js_generators_supported_script_function += counts.script_function;
        scan.requires_js_generators_supported_custom += counts.custom;
    }

    Ok(scan)
}

/// Result of walking a parsed spec to classify every `requires_js` generator
/// into supported/unsupported buckets. `supported` is the subset of `total`,
/// further broken down by `js_runtime.kind` class.
#[derive(Debug, Default, Clone, Copy, Serialize)]
struct JsClassCounts {
    total: usize,
    supported: usize,
    /// `requires_js` generators whose `js_runtime.kind == post_process`
    /// AND that carry an accompanying `script` / `script_template`. These
    /// are dispatched through the QuickJS post-process pipeline.
    post_process: usize,
    /// `requires_js` generators whose `js_runtime.kind == script_function`.
    /// JS body evaluates to an `argv` array, then spawned.
    script_function: usize,
    /// `requires_js` generators whose `js_runtime.kind == custom`. JS body
    /// returns suggestions directly without a subprocess.
    custom: usize,
}

/// Walk a raw `serde_json::Value` and classify every object with
/// `"requires_js": true` according to the runtime dispatch rules:
///
/// - `total` increments for every `requires_js: true` occurrence.
/// - `supported` increments when the generator carries supportable
///   `js_runtime` metadata. The class-specific fields
///   (`post_process` / `script_function` / `custom`) are mutually exclusive
///   counters that sum to `supported`.
///
/// This is the doctor/status-side mirror of the runtime classification in
/// `gc_suggest::specs::collect_generators` — keeping them in sync is a
/// load-bearing invariant for the coverage counters surfaced by
/// `status --json`.
fn count_requires_js_classes_in_value(value: &serde_json::Value) -> JsClassCounts {
    let mut stack: Vec<&serde_json::Value> = vec![value];
    let mut counts = JsClassCounts::default();
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(map) => {
                if matches!(map.get("requires_js"), Some(serde_json::Value::Bool(true))) {
                    counts.total += 1;
                    if let Some(kind) = supported_kind(map) {
                        counts.supported += 1;
                        match kind {
                            SupportedKind::PostProcess => counts.post_process += 1,
                            SupportedKind::ScriptFunction => counts.script_function += 1,
                            SupportedKind::Custom => counts.custom += 1,
                        }
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

/// Internal enum used to count supported generators by class while
/// keeping a single classifier as the source of truth.
#[derive(Debug, Clone, Copy)]
enum SupportedKind {
    PostProcess,
    ScriptFunction,
    Custom,
}

/// Returns the supported `js_runtime.kind` class when a generator object has
/// the shape the runtime can dispatch, or `None` otherwise.
///
/// Mirrors `collect_generators` in `gc-suggest::specs` and the dispatch
/// gate in `gc-suggest::engine::is_supported_script_generator`. The
/// engine handles all three `js_runtime.kind` variants, but with subtly
/// different gates:
///   * `post_process` requires an accompanying `script` / `script_template`
///     plus a non-empty `js_runtime.source`. `self_contained` is irrelevant
///     because the JS body only post-processes shell stdout — there is no
///     bundler-helper closure surface.
///   * `script_function` and `custom` need a non-empty `source` AND
///     `js_runtime.self_contained == true`. The latter is the converter's
///     proof that the embedded JS does not close over bundler/minifier
///     helper bindings (`__exports__`, `__webpack_require__`, etc.) that
///     the QuickJS host will not install. Any generator without
///     `self_contained: true` is therefore dropped into the unsupported
///     bucket — the same truthful state the engine actually dispatches
///     against. (The bucket size is a function of how many generators
///     the converter has been able to prove self-contained for and
///     fluctuates between releases; see CHANGELOG.md for the snapshot
///     count at any given version.)
fn supported_kind(map: &serde_json::Map<String, serde_json::Value>) -> Option<SupportedKind> {
    let runtime = map.get("js_runtime").and_then(|v| v.as_object())?;
    let kind = runtime
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or_default();
    let source_non_empty = runtime
        .get("source")
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    // `JsRuntimeSpec.self_contained` carries `#[serde(default)]` (default
    // `false`) so a missing key here behaves identically to the typed
    // deserialiser — we MUST NOT assume an absent field means `true`.
    let self_contained_true = runtime
        .get("self_contained")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match kind {
        "post_process" => {
            let has_script = map
                .get("script")
                .map(|v| v.is_array() || v.is_string())
                .unwrap_or(false);
            let has_template = map
                .get("script_template")
                .map(|v| v.is_array() || v.is_string())
                .unwrap_or(false);
            if (has_script || has_template) && source_non_empty {
                Some(SupportedKind::PostProcess)
            } else {
                None
            }
        }
        "script_function" if source_non_empty && self_contained_true => {
            Some(SupportedKind::ScriptFunction)
        }
        "custom" if source_non_empty && self_contained_true => Some(SupportedKind::Custom),
        _ => None,
    }
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

    // Coverage / dynamic-generator / addressability / runtime sections.
    // Always rendered — even when zero specs are loaded — so users
    // running on a fresh box see what classification each metric means.
    // The text format mirrors `status --json` field names so users can
    // move between the two views without re-learning labels.
    let nonfunctional = outcome.commands_nonfunctional;
    writeln!(out, "\nCoverage:")?;
    writeln!(
        out,
        "  Fully functional:           {} commands",
        outcome.fully_functional
    )?;
    writeln!(
        out,
        "  Partially functional:       {} commands  (have requires_js generators)",
        outcome.partially_functional
    )?;
    writeln!(
        out,
        "  Nonfunctional:              {} commands",
        nonfunctional
    )?;

    writeln!(out, "\nDynamic generators (requires_js):")?;
    writeln!(
        out,
        "  Total:                      {}",
        outcome.requires_js_generators_total
    )?;
    writeln!(
        out,
        "  Supported (post_process):   {}",
        outcome
            .file_scan
            .requires_js_generators_supported_post_process
    )?;
    writeln!(
        out,
        "  Supported (script_function):{}",
        outcome
            .file_scan
            .requires_js_generators_supported_script_function
    )?;
    writeln!(
        out,
        "  Supported (custom):         {}",
        outcome.file_scan.requires_js_generators_supported_custom
    )?;
    writeln!(
        out,
        "  Unsupported:                {}",
        outcome.requires_js_generators_unsupported
    )?;

    let unique_entries = outcome.file_scan.spec_files_total;
    let actionable_conflicts = outcome
        .command_alias_conflict_details
        .iter()
        .filter(|c| c.kind != "directory_precedence")
        .count();
    let directory_overrides = outcome
        .command_alias_conflict_details
        .iter()
        .filter(|c| c.kind == "directory_precedence")
        .count();
    writeln!(out, "\nCommand addressability:")?;
    writeln!(out, "  Unique entries:             {}", unique_entries)?;
    writeln!(
        out,
        "  Aliases:                    {}",
        outcome.commands_addressable
    )?;
    writeln!(
        out,
        "  Conflicts:                  {}  (duplicate/name-stem aliases that lose)",
        actionable_conflicts
    )?;
    if directory_overrides > 0 {
        writeln!(
            out,
            "  Directory overrides:        {}  (earlier spec_dir preferred; fallback may parse)",
            directory_overrides
        )?;
    }

    writeln!(out, "\nJS runtime:")?;
    if outcome.js_runtime_enabled {
        writeln!(
            out,
            "  Status: \x1b[32menabled\x1b[0m (suggest.providers.js_runtime = true)"
        )?;
    } else {
        writeln!(
            out,
            "  Status: \x1b[33mdisabled\x1b[0m \u{2014} set suggest.providers.js_runtime = true to re-enable"
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
/// 1.0 — original shape.
/// 1.1 — adds `commands_addressable`,
///       `commands_(fully|partially|non)functional`,
///       `requires_js_generators_(total|supported|unsupported)`,
///       `command_alias_conflicts` to `spec_counts`, plus a top-level
///       `file_scan` block. All additions are purely additive — old
///       fields keep their meaning so existing JSON consumers still
///       parse the output unchanged.
/// 1.2 — serialises individual alias conflict records as a structured
///       `command_alias_conflict_details` array (each entry is an object
///       with `alias`, `kind`, `winner_*`, `loser_*` fields), splits
///       `requires_js_generators_supported` into a per-kind class
///       breakdown under `requires_js_generators_supported_by_kind`
///       (`post_process`, `script_function`, `custom`), and surfaces
///       the `js_runtime` kill switch under a top-level `js_runtime`
///       block (`enabled: bool`). All additions are purely additive —
///       1.1 consumers still parse 1.2 output unchanged.
/// 1.3 — updates counter semantics: `command_alias_conflicts` can include
///       lower-precedence lazy fallback candidates, `commands_nonfunctional`
///       counts aliases whose entire fallback chain fails lazy parsing, and
///       `file_scan.spec_files_total` counts resolved runtime sources after
///       lazy fallback resolution instead of raw file-level scan entries.
///       `command_alias_conflict_details` entries also include a
///       `disposition` field so consumers can distinguish rejected aliases
///       from fallback candidates.
const STATUS_SCHEMA_VERSION: &str = "1.3";

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
    /// Raw-JSON scan over resolved runtime sources. Counts generator
    /// occurrences while following lazy fallback and avoiding hidden
    /// candidate double-counts.
    file_scan: FileScan,
    /// JS runtime kill switch state. Reflects
    /// `suggest.providers.js_runtime`. `enabled = false` means the
    /// engine will not dispatch any requires_js generators even if their
    /// metadata is fully populated.
    js_runtime: JsRuntimeStatus,
    coverage_trend: Option<CoverageTrend>,
}

/// JSON block for the JS runtime kill switch state.
#[derive(Debug, Serialize)]
struct JsRuntimeStatus {
    enabled: bool,
}

/// Counters surfaced under `spec_counts`. Schema 1.1 added the
/// `command_*` and `requires_js_generators_*` fields; the legacy fields
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
    /// Numerically identical to `fully_functional`. Kept as a separate
    /// field so consumers can distinguish "spec is fully functional" from
    /// "command-level rollup is fully functional" once the definitions
    /// diverge (e.g. when partially-functional commands whose requires_js
    /// generators all activate get promoted into the fully-functional
    /// bucket).
    commands_fully_functional: usize,
    commands_partially_functional: usize,
    /// Command aliases for which every registered candidate failed lazy
    /// parsing. Directory scan failures and parse failures masked by a valid
    /// fallback stay in `parse_errors`; alias conflicts are reported
    /// separately.
    commands_nonfunctional: usize,
    /// Total `requires_js: true` generator instances across resolved runtime
    /// spec JSON sources (counted per occurrence, not per spec). Sourced from
    /// a raw `serde_json::Value` walk — equivalent to
    /// `[.. | objects | select(.requires_js == true)] | length` over those
    /// sources — because the structured loader silently drops some generator
    /// slots (see `scan_spec_files`). Equal to
    /// `file_scan.requires_js_generators_total`.
    requires_js_generators_total: usize,
    /// Subset of `requires_js_generators_total` whose `js_runtime` metadata
    /// matches a shape the engine can dispatch (`post_process` with an
    /// accompanying script, `script_function`, or `custom`).
    requires_js_generators_supported: usize,
    /// `requires_js_generators_total - requires_js_generators_supported`.
    /// Surfaced as its own field so consumers don't need to subtract.
    requires_js_generators_unsupported: usize,
    /// Runtime alias collisions surfaced by the loader. SpecStore keys on
    /// filename stem (canonical id) plus the spec's `name` field as a
    /// secondary alias when free; an entry here can be either a rejected alias
    /// (DuplicateName / NameMatchesOtherStem) or a registered fallback
    /// candidate behind a higher-precedence owner. Each conflict carries
    /// source-dir + alias diagnostics in `SpecStore::conflicts()`. The
    /// structured per-conflict breakdown is exposed under
    /// `command_alias_conflict_details`; this count remains in 1.1 for
    /// backwards compat.
    command_alias_conflicts: usize,
    /// Per-conflict structured details. Each entry is an object with `alias`,
    /// `kind`, `disposition`, `winner_*`, `loser_*` fields. `kind` is one of
    /// `duplicate_name`, `name_matches_other_stem`, `directory_precedence`.
    /// Always present (empty when no conflicts).
    command_alias_conflict_details: Vec<AliasConflictRecord>,
    /// Class breakdown of `requires_js_generators_supported` by
    /// `js_runtime.kind`. Three numeric fields: `post_process`
    /// (post_process+script lowering), `script_function`, `custom`. Sums to
    /// `requires_js_generators_supported`.
    requires_js_generators_supported_by_kind: SupportedByKind,
}

/// Per-class breakdown of supported requires_js generators.
#[derive(Debug, Serialize)]
struct SupportedByKind {
    post_process: usize,
    script_function: usize,
    custom: usize,
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
    // `filesystem_overrides` is the count of resolved filesystem specs after
    // configured spec_dirs are reduced through lazy fallback resolution.
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
            command_alias_conflict_details: outcome.command_alias_conflict_details.clone(),
            requires_js_generators_supported_by_kind: SupportedByKind {
                post_process: outcome
                    .file_scan
                    .requires_js_generators_supported_post_process,
                script_function: outcome
                    .file_scan
                    .requires_js_generators_supported_script_function,
                custom: outcome.file_scan.requires_js_generators_supported_custom,
            },
        },
        file_scan: outcome.file_scan.clone(),
        js_runtime: JsRuntimeStatus {
            enabled: outcome.js_runtime_enabled,
        },
        coverage_trend,
    };

    let s = serde_json::to_string_pretty(&payload).context("failed to serialize status JSON")?;
    writeln!(out, "{}", s)?;
    Ok(outcome)
}

/// Render the status report. When `strict` is `true`, prints the full report
/// first and then exits with code 1 if spec health is degraded — meaning any
/// of:
///   - zero parsed runtime specs are available (nothing to complete against),
///     or
///   - one or more spec directories failed to scan or spec files failed lazy
///     parsing.
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

    let exit =
        run_status_with_opts_to_writer(config_path, strict, json, baseline_path, &mut handle)?;
    if exit == StatusExit::Failure {
        std::process::exit(1);
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusExit {
    Success,
    Failure,
}

fn run_status_with_opts_to_writer(
    config_path: Option<&str>,
    strict: bool,
    json: bool,
    baseline_path: Option<&Path>,
    out: &mut dyn Write,
) -> Result<StatusExit> {
    let outcome = if json {
        run_status_json(config_path, baseline_path, out)?
    } else {
        run_status_inner_with_trend(config_path, baseline_path, out)?
    };

    if strict {
        let parsed_runtime_specs = outcome.fully_functional + outcome.partially_functional;
        let no_specs_available = parsed_runtime_specs == 0 && outcome.total_parse_errors == 0;
        if no_specs_available || outcome.total_parse_errors > 0 {
            if !json {
                writeln!(out)?;
                if no_specs_available {
                    writeln!(
                        out,
                        "\x1b[31mstrict mode: no runtime specs available.\x1b[0m"
                    )?;
                }
                if outcome.total_parse_errors > 0 {
                    writeln!(
                        out,
                        "\x1b[31mstrict mode: {} spec file(s) failed to parse.\x1b[0m",
                        outcome.total_parse_errors
                    )?;
                }
            }
            return Ok(StatusExit::Failure);
        }
    }

    Ok(StatusExit::Success)
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

    /// When two resolved spec_dirs ship copies of the same filename,
    /// the file-level walk MUST count only the entry SpecStore resolves at
    /// runtime, not every fallback candidate. Without the resolved-entry scan
    /// the second dir's `git.json` would re-enter the totals and double the
    /// requires_js count.
    #[test]
    fn status_file_scan_does_not_double_count_overlapping_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary_dir = tmp.path().join("primary-specs");
        let fallback_dir = tmp.path().join("fallback-specs");
        std::fs::create_dir_all(&primary_dir).unwrap();
        std::fs::create_dir_all(&fallback_dir).unwrap();

        // primary copy of git.json: 5 requires_js generators
        let primary_git = make_git_spec_with_requires_js(5);
        std::fs::write(primary_dir.join("git.json"), primary_git).unwrap();

        // fallback copy of git.json: 10 requires_js generators (would
        // dominate the corpus count if both copies were summed)
        let fallback_git = make_git_spec_with_requires_js(10);
        std::fs::write(fallback_dir.join("git.json"), fallback_git).unwrap();

        let cfg = write_config_for_dirs(&[&primary_dir, &fallback_dir], &tmp);
        let outcome = scan_specs(Some(cfg.to_str().unwrap())).unwrap();

        // SpecStore keeps fallback candidates, but only the primary entry
        // resolves while it parses successfully.
        assert_eq!(outcome.fs_specs, 1, "only one duplicate resolves");
        // file_scan now mirrors that — counts ONLY the primary file.
        assert_eq!(
            outcome.file_scan.spec_files_total, 1,
            "file scan walks the loader's resolved entries, not every dir"
        );
        assert_eq!(
            outcome.requires_js_generators_total, 5,
            "primary wins: only its 5 generators count, not the fallback's 10 (15 total \
             would indicate the pre-fix double-counting bug)"
        );
        assert_eq!(
            outcome.command_alias_conflicts, 1,
            "fallback copy is recorded as a DirectoryPrecedence fallback candidate"
        );
        assert_eq!(
            outcome.command_alias_conflict_details[0].disposition,
            "fallback_candidate"
        );
    }

    #[test]
    fn status_nonfunctional_commands_ignore_masked_fallback_parse_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary_dir = tmp.path().join("primary-specs");
        let fallback_dir = tmp.path().join("fallback-specs");
        std::fs::create_dir_all(&primary_dir).unwrap();
        std::fs::create_dir_all(&fallback_dir).unwrap();

        std::fs::write(primary_dir.join("git.json"), "{not valid json").unwrap();
        std::fs::write(
            fallback_dir.join("git.json"),
            r#"{"name":"git","subcommands":[{"name":"from-fallback"}]}"#,
        )
        .unwrap();

        let cfg = write_config_for_dirs(&[&primary_dir, &fallback_dir], &tmp);
        let outcome = scan_specs(Some(cfg.to_str().unwrap())).unwrap();

        assert_eq!(
            outcome.total_parse_errors, 1,
            "malformed higher-precedence duplicate should still be reported as a parse error"
        );
        assert_eq!(
            outcome.commands_addressable, 1,
            "only the git alias is addressable"
        );
        assert_eq!(
            outcome.commands_nonfunctional, 0,
            "git remains functional through the lower-precedence parsed candidate"
        );
        assert_eq!(
            outcome.fully_functional, 1,
            "fallback candidate should be the resolved runtime spec"
        );
    }

    #[test]
    fn status_file_scan_counts_embedded_runtime_entries() {
        let result = gc_suggest::SpecStore::load_with_embedded(&[]).unwrap();
        assert!(
            !result.store.is_empty(),
            "embedded-only runtime store must register entries"
        );

        let scan = scan_spec_files(&result.store).unwrap();

        assert_eq!(
            scan.spec_files_total,
            result.store.len(),
            "file_scan must count embedded entries from SpecSource::Embedded"
        );
        assert!(
            scan.requires_js_generators_total > 0,
            "embedded corpus should contribute requires_js totals"
        );
    }

    #[test]
    fn status_counts_embedded_runtime_store_when_no_dirs_resolved() {
        let config = gc_config::GhostConfig::default();

        let outcome = scan_resolved_specs(&config, &[], true).unwrap();

        assert_eq!(
            outcome.fs_specs, 0,
            "embedded fallback should not be reported as filesystem overrides"
        );
        assert!(
            outcome.fully_functional > 0,
            "embedded-only status should classify parsed runtime specs"
        );
        assert!(
            outcome.commands_addressable > 0,
            "embedded-only status should expose addressable command aliases"
        );
        assert!(
            outcome.file_scan.spec_files_total > 0,
            "embedded-only status should count embedded file_scan entries"
        );
        assert!(
            outcome.requires_js_generators_total > 0,
            "embedded-only status should report embedded requires_js totals"
        );
    }

    #[test]
    fn status_counts_resolved_filesystem_entry_once_with_embedded_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("specs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("git.json"),
            r#"{"name":"git","subcommands":[{"name":"from-filesystem"}],"options":[],"args":[]}"#,
        )
        .unwrap();

        let config = gc_config::GhostConfig::default();
        let outcome = scan_resolved_specs(&config, &[dir], true).unwrap();

        assert_eq!(outcome.fs_specs, 1);
        assert_eq!(
            outcome.fully_functional + outcome.partially_functional,
            outcome.embedded_count,
            "filesystem git should replace embedded git in resolved counts, not add to it"
        );
        assert_eq!(
            outcome.file_scan.spec_files_total, outcome.embedded_count,
            "file_scan should count resolved sources, not hidden fallback candidates"
        );
    }

    #[test]
    fn status_strict_returns_failure_for_lazy_parse_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("broken.json"), "{not valid json").unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let exit = run_status_with_opts_to_writer(
            Some(cfg.to_str().unwrap()),
            true,
            false,
            None,
            &mut out,
        )
        .unwrap();
        let txt = String::from_utf8_lossy(&out);

        assert_eq!(exit, StatusExit::Failure);
        assert!(
            txt.contains("broken.json: shallow parse") && txt.contains("header:"),
            "strict status should print the lazy parse failure line:\n{txt}"
        );
        assert!(
            txt.contains("strict mode: 1 spec file(s) failed to parse."),
            "strict status should explain the non-zero exit:\n{txt}"
        );
    }

    #[test]
    fn status_lazy_parse_errors_include_filesystem_source_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        let broken_path = spec_dir.join("broken.json");
        std::fs::write(&broken_path, "{not valid json").unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let outcome = scan_specs(Some(cfg.to_str().unwrap())).unwrap();

        assert_eq!(outcome.total_parse_errors, 1);
        let detail = outcome.parse_error_lines.first().unwrap();
        assert!(
            detail.contains(&broken_path.display().to_string()),
            "lazy parse error should include filesystem source path, got: {detail}"
        );
    }

    #[test]
    fn status_strict_returns_success_for_clean_runtime_specs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("ok.json"), r#"{"name":"ok"}"#).unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        let exit = run_status_with_opts_to_writer(
            Some(cfg.to_str().unwrap()),
            true,
            false,
            None,
            &mut out,
        )
        .unwrap();

        assert_eq!(exit, StatusExit::Success);
    }

    #[test]
    fn status_human_counts_directory_precedence_as_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary_dir = tmp.path().join("primary-specs");
        let fallback_dir = tmp.path().join("fallback-specs");
        std::fs::create_dir_all(&primary_dir).unwrap();
        std::fs::create_dir_all(&fallback_dir).unwrap();
        std::fs::write(primary_dir.join("git.json"), r#"{"name":"git"}"#).unwrap();
        std::fs::write(fallback_dir.join("git.json"), r#"{"name":"git"}"#).unwrap();

        let cfg = write_config_for_dirs(&[&primary_dir, &fallback_dir], &tmp);
        let mut out = Vec::new();
        run_status_inner(Some(cfg.to_str().unwrap()), &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);

        assert!(
            txt.contains("Conflicts:                  0"),
            "directory precedence should not render as an actionable conflict:\n{txt}"
        );
        assert!(
            txt.contains("Directory overrides:        1"),
            "directory precedence should be visible as an override:\n{txt}"
        );
    }

    /// Build a minimal git-style spec body with `n` requires_js generators
    /// across distinct positional args. Each generator is shaped exactly
    /// like the runtime classifier expects (no js_runtime metadata, so
    /// they all land in the unsupported bucket).
    fn make_git_spec_with_requires_js(n: usize) -> String {
        let args: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    r#"{{"name":"a{i}","generators":[{{"script":["echo","x"],"requires_js":true}}]}}"#
                )
            })
            .collect();
        format!(r#"{{"name":"git","args":[{}]}}"#, args.join(","))
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

        assert_eq!(parsed["schema_version"], "1.3");
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
        // schema 1.1 additions.
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
        // Structured conflict/runtime additions.
        assert!(
            parsed["spec_counts"]["command_alias_conflict_details"].is_array(),
            "command_alias_conflict_details must be an array"
        );
        assert!(
            parsed["spec_counts"]["requires_js_generators_supported_by_kind"].is_object(),
            "requires_js_generators_supported_by_kind must be an object"
        );
        assert!(
            parsed["spec_counts"]["requires_js_generators_supported_by_kind"]["post_process"]
                .is_number()
        );
        assert!(
            parsed["spec_counts"]["requires_js_generators_supported_by_kind"]["script_function"]
                .is_number()
        );
        assert!(
            parsed["spec_counts"]["requires_js_generators_supported_by_kind"]["custom"].is_number()
        );
        assert!(
            parsed["js_runtime"].is_object(),
            "js_runtime top-level block must be present"
        );
        assert!(parsed["js_runtime"]["enabled"].is_boolean());
        assert!(parsed["file_scan"]["requires_js_generators_supported_post_process"].is_number());
        assert!(
            parsed["file_scan"]["requires_js_generators_supported_script_function"].is_number()
        );
        assert!(parsed["file_scan"]["requires_js_generators_supported_custom"].is_number());
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

    /// `status --json` schema 1.1 must expose every counter field. This
    /// test exercises the wiring without depending on the corpus content
    /// (uses two synthetic fixtures so the numbers are pinned).
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

        // Current schema surfaces every command and generator counter as
        // a numeric value.
        assert_eq!(parsed["schema_version"], "1.3");
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
            "fixture predates js_runtime metadata so it stays unsupported"
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

    #[test]
    fn malformed_specs_are_nonfunctional_and_fail_coverage_gate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("broken.json"), "{not valid json").unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let baseline = write_baseline(
            &tmp,
            r#"{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "test-fixture",
      "timestamp": "2026-05-03T00:00:00Z",
      "total_specs": 0,
      "fully_functional": 0,
      "requires_js_generators": 0,
      "native_providers": 0,
      "corrected_generators": 0,
      "hand_audit_required": 0,
      "requires_js_generators_total": 0,
      "requires_js_generators_supported": 0,
      "requires_js_generators_unsupported": 0
    }
  ]
}"#,
        );

        let mut out = Vec::new();
        run_status_json(Some(cfg.to_str().unwrap()), Some(&baseline), &mut out).unwrap();
        let status_path = tmp.path().join("status.json");
        std::fs::write(&status_path, &out).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["spec_counts"]["parse_errors"].as_u64().unwrap(), 1);
        assert!(
            parsed["spec_counts"]["commands_nonfunctional"]
                .as_u64()
                .unwrap()
                > 0,
            "malformed specs must make status report nonfunctional commands: {parsed}"
        );

        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let script = repo_root.join("scripts/check-coverage-regression.sh");
        let output = std::process::Command::new("bash")
            .arg(script)
            .arg("--baseline")
            .arg(&baseline)
            .arg("--status-json")
            .arg(&status_path)
            .output()
            .unwrap();

        assert_eq!(
            output.status.code(),
            Some(1),
            "coverage gate should fail on real status output with a malformed spec\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("command(s) are nonfunctional"),
            "gate failure should name the nonfunctional-command count, stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// `commands_partially_functional` is the count of specs where at
    /// least one generator carries `requires_js: true`. The runtime
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

    /// The active fixtures parse cleanly and produce the expected counter
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

        // The active fixture set:
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
        //   post_process_supported.json         → stem `post_process_supported`,
        //                                          name `post-process-supported`,
        //                                          partially functional, 1 requires_js
        //                                          (js_runtime.kind = post_process).
        //   custom_unsupported.json             → stem `custom_unsupported`,
        //                                          name `custom-unsupported`,
        //                                          partially functional, 1 requires_js
        //                                          (js_runtime.kind = custom WITHOUT
        //                                          `self_contained: true`). The engine
        //                                          gates `script_function`/`custom`
        //                                          dispatch on the converter-emitted
        //                                          `self_contained` proof, so this
        //                                          fixture lives up to its name and
        //                                          stays in the unsupported bucket.
        //
        // file_scan sees all 7 files; SpecStore keeps all 7 entries (filename
        // stems unique). commands_addressable counts the 7 stems plus 6
        // non-conflicting name aliases (`static-only`, `partial-unsupported-js`,
        // `alias-target`, `duplicate`, `post-process-supported`,
        // `custom-unsupported`).
        assert_eq!(outcome.file_scan.spec_files_total, 7);
        assert_eq!(
            outcome.fs_specs, 7,
            "every committed file is a unique entry"
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
        // Only `post_process_supported` lands in the supported bucket:
        //   * `post_process_supported` — kind=post_process, has script,
        //     non-empty source → supported.
        //   * `custom_unsupported` — kind=custom but NO `self_contained:
        //     true`. The engine refuses to dispatch unproven custom
        //     sources (see gc_suggest::engine::is_supported_script_generator)
        //     so the status counter must mirror that decision.
        //   * `partial_unsupported_js` predates `js_runtime` metadata
        //     entirely so it stays in the unsupported bucket.
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

    // -------------------------------------------------------------------------
    // text-mode coverage sections + current JSON alias conflict fields.
    // -------------------------------------------------------------------------

    /// Text mode renders the Coverage / Dynamic generators / Command
    /// addressability / JS runtime sections with the right counts.
    #[test]
    fn status_text_mode_renders_coverage_section() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        // One static spec + one with a supported post_process generator
        // + one with an unsupported requires_js (no js_runtime). Yields:
        //   fully_functional: 1
        //   partially_functional: 2
        //   total: 2 generators
        //   supported: 1 (post_process)
        //   unsupported: 1
        std::fs::write(
            spec_dir.join("static.json"),
            r#"{"name":"static","subcommands":[{"name":"go"}]}"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("post.json"),
            r#"{
                "name": "post",
                "args": [{
                    "name": "x",
                    "generators": [{
                        "script": ["cmd"],
                        "requires_js": true,
                        "js_runtime": {"kind":"post_process","source":"() => []"}
                    }]
                }]
            }"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("legacy.json"),
            r#"{
                "name": "legacy",
                "args": [{
                    "name": "x",
                    "generators": [{"requires_js": true, "js_source": "(()=>[])"}]
                }]
            }"#,
        )
        .unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_status_inner(Some(cfg.to_str().unwrap()), &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);

        // Coverage section
        assert!(
            txt.contains("Coverage:"),
            "expected Coverage header, got:\n{txt}"
        );
        assert!(
            txt.contains("Fully functional:           1 commands"),
            "expected fully functional count line, got:\n{txt}"
        );
        assert!(
            txt.contains("Partially functional:       2 commands"),
            "expected partially functional count line, got:\n{txt}"
        );
        assert!(
            txt.contains("Nonfunctional:              0 commands"),
            "expected nonfunctional count line, got:\n{txt}"
        );

        // Dynamic generators section
        assert!(
            txt.contains("Dynamic generators (requires_js):"),
            "expected dynamic generators header, got:\n{txt}"
        );
        assert!(
            txt.contains("Total:                      2"),
            "expected dynamic-generator total, got:\n{txt}"
        );
        assert!(
            txt.contains("Supported (post_process):   1"),
            "expected post_process count, got:\n{txt}"
        );
        assert!(
            txt.contains("Unsupported:                1"),
            "expected unsupported count, got:\n{txt}"
        );

        // Command addressability
        assert!(
            txt.contains("Command addressability:"),
            "expected addressability header, got:\n{txt}"
        );
        assert!(
            txt.contains("Unique entries:             3"),
            "expected unique entries count, got:\n{txt}"
        );

        // JS runtime
        assert!(
            txt.contains("JS runtime:"),
            "expected JS runtime header, got:\n{txt}"
        );
        assert!(
            txt.contains("enabled"),
            "default config is enabled, got:\n{txt}"
        );
    }

    /// When the kill switch is off, text output names the disabled state
    /// and points at the config key to flip back on.
    #[test]
    fn status_text_mode_shows_disabled_runtime() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        let cfg_path = tmp.path().join("config.toml");
        let body = format!(
            "[paths]\nspec_dirs = [\"{}\"]\n[suggest.providers]\njs_runtime = false\n",
            spec_dir.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(&cfg_path, body).unwrap();

        let mut out = Vec::new();
        run_status_inner(Some(cfg_path.to_str().unwrap()), &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);

        assert!(
            txt.contains("disabled"),
            "expected `disabled` token in JS runtime section, got:\n{txt}"
        );
        assert!(
            txt.contains("suggest.providers.js_runtime = true"),
            "expected pointer at the config key, got:\n{txt}"
        );
    }

    /// Current JSON output exposes the structured alias conflict list and
    /// the per-kind breakdown.
    #[test]
    fn status_json_includes_alias_conflicts_breakdown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        // Two specs declaring the same `name` — the second remains behind
        // the primary owner as a lazy fallback and surfaces a `DuplicateName`
        // conflict.
        std::fs::write(spec_dir.join("a.json"), r#"{"name": "duplicate"}"#).unwrap();
        std::fs::write(spec_dir.join("b.json"), r#"{"name": "duplicate"}"#).unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_status_json(Some(cfg.to_str().unwrap()), None, &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);
        let parsed: serde_json::Value = serde_json::from_str(&txt).unwrap();

        assert_eq!(parsed["schema_version"], "1.3");
        let details = parsed["spec_counts"]["command_alias_conflict_details"]
            .as_array()
            .expect("command_alias_conflict_details must be an array");
        assert_eq!(details.len(), 1, "expected one conflict, got {details:?}");
        let entry = &details[0];
        assert_eq!(entry["alias"], "duplicate");
        assert_eq!(entry["kind"], "duplicate_name");
        assert_eq!(entry["disposition"], "fallback_candidate");
        // Either spec stem may win depending on dir-walk order; both are
        // valid as long as winner != loser.
        assert_ne!(entry["winner_stem"], entry["loser_stem"]);

        // Per-kind breakdown is present even when zero supported.
        let by_kind = &parsed["spec_counts"]["requires_js_generators_supported_by_kind"];
        assert_eq!(by_kind["post_process"].as_u64().unwrap(), 0);
        assert_eq!(by_kind["script_function"].as_u64().unwrap(), 0);
        assert_eq!(by_kind["custom"].as_u64().unwrap(), 0);

        // js_runtime kill switch surfaces (default true).
        assert_eq!(
            parsed["js_runtime"]["enabled"],
            serde_json::Value::Bool(true)
        );
    }

    /// The per-kind breakdown sums to `requires_js_generators_supported`,
    /// including non-trivial mixes.
    /// Each non-PostProcess fixture carries `self_contained: true` so
    /// the engine's dispatch gate accepts it — without that flag the
    /// engine's `is_supported_script_generator` predicate (and
    /// therefore the status mirror) treat it as unsupported. See
    /// `script_function_without_self_contained_is_unsupported` for the
    /// negative direction.
    #[test]
    fn status_json_per_kind_breakdown_sums() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        // One post_process + one script_function + one custom + one
        // unsupported. Total 4, supported 3 (1+1+1).
        std::fs::write(
            spec_dir.join("a.json"),
            r#"{"name":"a","args":[{"name":"x","generators":[{
                "script": ["cmd"],
                "requires_js": true,
                "js_runtime": {"kind":"post_process","source":"()=>[]"}
            }]}]}"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("b.json"),
            r#"{"name":"b","args":[{"name":"x","generators":[{
                "requires_js": true,
                "js_runtime": {"kind":"script_function","source":"()=>[]","self_contained":true}
            }]}]}"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("c.json"),
            r#"{"name":"c","args":[{"name":"x","generators":[{
                "requires_js": true,
                "js_runtime": {"kind":"custom","source":"()=>[]","self_contained":true}
            }]}]}"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("d.json"),
            r#"{"name":"d","args":[{"name":"x","generators":[{
                "requires_js": true
            }]}]}"#,
        )
        .unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_status_json(Some(cfg.to_str().unwrap()), None, &mut out).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out)).unwrap();

        let counts = &parsed["spec_counts"];
        assert_eq!(counts["requires_js_generators_total"].as_u64().unwrap(), 4);
        assert_eq!(
            counts["requires_js_generators_supported"].as_u64().unwrap(),
            3
        );
        assert_eq!(
            counts["requires_js_generators_unsupported"]
                .as_u64()
                .unwrap(),
            1
        );

        let by = &counts["requires_js_generators_supported_by_kind"];
        assert_eq!(by["post_process"].as_u64().unwrap(), 1);
        assert_eq!(by["script_function"].as_u64().unwrap(), 1);
        assert_eq!(by["custom"].as_u64().unwrap(), 1);
    }

    /// Regression guard for code-1: the engine
    /// (gc-suggest::engine::is_supported_script_generator and
    /// specs::collect_generators) gates `script_function` / `custom`
    /// dispatch on `js_runtime.self_contained == true`. Without that
    /// proof the engine silently skips the generator. The status
    /// mirror MUST report the same — otherwise the coverage gate
    /// (`scripts/check-coverage-regression.sh`) reads false-100% and
    /// can never detect a regression.
    #[test]
    fn script_function_without_self_contained_is_unsupported() {
        // Non-self-contained `script_function` — must NOT be classified
        // as supported.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "script_function",
                    "source": "() => ['a']"
                }
            }"#,
        )
        .unwrap();
        assert!(
            super::supported_kind(&map).is_none(),
            "script_function without self_contained:true must be unsupported"
        );

        // Same shape with `self_contained:false` — same outcome.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "script_function",
                    "source": "() => ['a']",
                    "self_contained": false
                }
            }"#,
        )
        .unwrap();
        assert!(
            super::supported_kind(&map).is_none(),
            "script_function with self_contained:false must be unsupported"
        );

        // `custom` mirrors the same gate.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "custom",
                    "source": "async () => [{name: 'a'}]"
                }
            }"#,
        )
        .unwrap();
        assert!(
            super::supported_kind(&map).is_none(),
            "custom without self_contained:true must be unsupported"
        );

        // Positive control: `self_contained: true` lifts the same shape
        // back into the supported bucket.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "script_function",
                    "source": "() => ['a']",
                    "self_contained": true
                }
            }"#,
        )
        .unwrap();
        assert!(
            matches!(
                super::supported_kind(&map),
                Some(super::SupportedKind::ScriptFunction)
            ),
            "script_function with self_contained:true must be supported"
        );

        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "custom",
                    "source": "async () => [{name: 'a'}]",
                    "self_contained": true
                }
            }"#,
        )
        .unwrap();
        assert!(
            matches!(
                super::supported_kind(&map),
                Some(super::SupportedKind::Custom)
            ),
            "custom with self_contained:true must be supported"
        );

        // post_process keeps its existing gate (script + non-empty source);
        // `self_contained` is irrelevant because the JS only handles
        // shell stdout.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "script": ["cmd"],
                "requires_js": true,
                "js_runtime": {
                    "kind": "post_process",
                    "source": "out => out.split('\n')"
                }
            }"#,
        )
        .unwrap();
        assert!(
            matches!(
                super::supported_kind(&map),
                Some(super::SupportedKind::PostProcess)
            ),
            "post_process must remain supported without self_contained"
        );
    }

    /// Regression guard for test-iter3-1: `supported_kind` reads
    /// `self_contained` via raw `Value::as_bool()`, which silently
    /// returns `None` for any non-bool JSON type. A future converter
    /// regression that emits a JS-truthy non-bool (e.g. `1`, `"true"`,
    /// `null`) for `self_contained` MUST land the generator in the
    /// unsupported bucket — otherwise the coverage gate
    /// (`scripts/check-coverage-regression.sh`) reads a fake-100%
    /// baseline. The companion serde-typed path in doctor.rs already
    /// rejects these (the spec is dropped from `SpecStore` entirely),
    /// so this test pins the loose-JSON path to the same outcome —
    /// keeping the two surfaces symmetric under malformed input.
    #[test]
    fn supported_kind_treats_non_bool_self_contained_as_unsupported() {
        // `self_contained: "true"` — JS-truthy string, NOT a boolean.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "script_function",
                    "source": "() => ['a']",
                    "self_contained": "true"
                }
            }"#,
        )
        .unwrap();
        assert!(
            super::supported_kind(&map).is_none(),
            "self_contained as a string must be treated as unsupported"
        );

        // `self_contained: 1` — JS-truthy number, NOT a boolean.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "script_function",
                    "source": "() => ['a']",
                    "self_contained": 1
                }
            }"#,
        )
        .unwrap();
        assert!(
            super::supported_kind(&map).is_none(),
            "self_contained as a number must be treated as unsupported"
        );

        // `self_contained: null` — explicitly null, NOT a boolean.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "script_function",
                    "source": "() => ['a']",
                    "self_contained": null
                }
            }"#,
        )
        .unwrap();
        assert!(
            super::supported_kind(&map).is_none(),
            "self_contained as null must be treated as unsupported"
        );

        // `custom` mirrors the same gate.
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "custom",
                    "source": "async () => [{name: 'a'}]",
                    "self_contained": "true"
                }
            }"#,
        )
        .unwrap();
        assert!(
            super::supported_kind(&map).is_none(),
            "custom with non-bool self_contained must be unsupported"
        );
    }
}
