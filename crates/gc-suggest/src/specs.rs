use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::providers::{self, ProviderKind};
use crate::transform::Transform;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};
use gc_buffer::CommandContext;

/// Native generator `type` strings that `git_generators_from` and the
/// filesystem templates actually recognize. Anything outside this list is
/// treated as unknown at load time — logged once so misconfigured specs
/// don't silently produce zero completions. Kept in sync with
/// `git::generator_to_query_kind` and the filepaths/folders template
/// handling in `collect_generators`.
pub(crate) const KNOWN_NATIVE_GENERATOR_TYPES: &[&str] = &[
    "git_branches",
    "git_tags",
    "git_remotes",
    "git_files",
    "filepaths",
    "folders",
];

/// Maximum nesting depth permitted in a spec JSON document. The deepest
/// real-world spec (atlas.json) is depth 7; capping at 32 leaves comfortable
/// headroom for legitimate growth while rejecting attacker-crafted input that
/// would otherwise stack-overflow downstream walkers (or serde_json's own
/// recursive parser, whose default limit of 128 is too generous for our
/// fixed-shape spec format).
pub const MAX_SPEC_JSON_DEPTH: usize = 32;

/// Reject JSON that nests `[`/`{` deeper than `max_depth`. Runs as a flat
/// byte scan over the source — no recursion, no allocation, and crucially no
/// dependency on the structure of the spec types. Done before handing the
/// bytes to `serde_json::from_str` so a malicious spec cannot exhaust the
/// stack inside the parser.
pub fn check_json_depth(src: &str, max_depth: usize) -> Result<()> {
    let bytes = src.as_bytes();
    let mut depth: usize = 0;
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > max_depth {
                    anyhow::bail!(
                        "spec exceeds maximum JSON nesting depth of {max_depth}; \
                         this is almost certainly a malformed or malicious spec"
                    );
                }
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// Strip control characters from text loaded from external specs. Mirrors
/// the policy applied at popup render time in `gc_overlay::render` —
/// defense in depth so an attacker-writable spec cannot inject CSI/OSC
/// sequences via `name` or `description`. The two crates can't easily share
/// this helper without breaking the existing dependency cycle
/// (`gc-overlay → gc-suggest → gc-config → gc-overlay`); inlining here is
/// the cheapest fix that keeps both sites consistent.
fn sanitize_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// Fast pre-check: byte-scan for any control codepoint. Catches C0 (0x00–0x1F
/// and 0x7F) directly and C1 (U+0080..=U+009F, encoded as 0xC2 0x80..=0xC2 0x9F
/// in UTF-8) via a two-byte match. Avoids per-char UTF-8 decoding + Unicode
/// table lookups, which dominated `load_from_dir` when `sanitize_spec_strings`
/// walked every string in 717 specs.
fn has_control_char(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x20 || b == 0x7F {
            return true;
        }
        if b == 0xC2 {
            if let Some(&next) = bytes.get(i + 1) {
                if (0x80..=0x9F).contains(&next) {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

fn sanitize_string(text: &mut String) {
    if has_control_char(text) {
        *text = sanitize_text(text);
    }
}

fn sanitize_opt(text: &mut Option<String>) {
    if let Some(s) = text.as_ref() {
        if has_control_char(s) {
            *text = Some(sanitize_text(s));
        }
    }
}

fn sanitize_arg_spec(arg: &mut ArgSpec) {
    sanitize_opt(&mut arg.name);
    sanitize_opt(&mut arg.description);
}

fn sanitize_option_spec(opt: &mut OptionSpec) {
    sanitize_opt(&mut opt.description);
    for n in &mut opt.name {
        sanitize_string(n);
    }
    if let Some(ref mut arg) = opt.args {
        sanitize_arg_spec(arg);
    }
}

/// Walk the spec tree iteratively and strip control characters from every
/// user-visible string field. Iteration (rather than recursion) avoids
/// re-introducing the recursion-depth attack surface this whole pass is
/// meant to remove.
pub fn sanitize_spec_strings(spec: &mut CompletionSpec) {
    sanitize_string(&mut spec.name);
    sanitize_opt(&mut spec.description);
    for arg in &mut spec.args {
        sanitize_arg_spec(arg);
    }
    for opt in &mut spec.options {
        sanitize_option_spec(opt);
    }

    let mut stack: Vec<&mut SubcommandSpec> = spec.subcommands.iter_mut().collect();
    while let Some(sub) = stack.pop() {
        sanitize_string(&mut sub.name);
        sanitize_opt(&mut sub.description);
        for arg in &mut sub.args {
            sanitize_arg_spec(arg);
        }
        for opt in &mut sub.options {
            sanitize_option_spec(opt);
        }
        stack.extend(sub.subcommands.iter_mut());
    }
}

/// Deserialize `args` as either a single object or an array of objects.
fn deserialize_args_one_or_many<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<ArgSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(ArgSpec),
        Many(Vec<ArgSpec>),
    }

    match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(single) => Ok(vec![single]),
        OneOrMany::Many(vec) => Ok(vec),
    }
}

/// Deserialize option `args` as either a single object or an array (taking the first).
fn deserialize_option_args<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ArgSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(ArgSpec),
        Many(Vec<ArgSpec>),
    }

    match Option::<OneOrMany>::deserialize(deserializer)? {
        Some(OneOrMany::One(single)) => Ok(Some(single)),
        Some(OneOrMany::Many(vec)) => Ok(vec.into_iter().next()),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompletionSpec {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default, deserialize_with = "deserialize_args_one_or_many")]
    pub args: Vec<ArgSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubcommandSpec {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default, deserialize_with = "deserialize_args_one_or_many")]
    pub args: Vec<ArgSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OptionSpec {
    pub name: Vec<String>,
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_option_args")]
    pub args: Option<ArgSpec>,
}

/// Deserialize template as either a single string or an array of strings.
/// When an array, takes the most useful entry: "filepaths" > "folders" > first.
fn deserialize_template<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    match Option::<OneOrMany>::deserialize(deserializer)? {
        Some(OneOrMany::One(s)) => Ok(Some(s)),
        Some(OneOrMany::Many(vec)) => {
            // Prefer "filepaths" over "folders" when both present
            if vec.iter().any(|t| t == "filepaths") {
                Ok(Some("filepaths".to_string()))
            } else {
                Ok(vec.into_iter().next())
            }
        }
        None => Ok(None),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArgSpec {
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub generators: Vec<GeneratorSpec>,
    #[serde(default, deserialize_with = "deserialize_template")]
    pub template: Option<String>,
    /// Static suggestions — accepted from specs but not yet used at runtime.
    #[serde(default)]
    pub suggestions: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub ttl_seconds: u64,
    #[serde(default)]
    pub cache_by_directory: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratorSpec {
    #[serde(rename = "type")]
    pub generator_type: Option<String>,
    pub script: Option<Vec<String>>,
    pub script_template: Option<Vec<String>>,
    #[serde(default)]
    pub transforms: Vec<Transform>,
    pub cache: Option<CacheConfig>,
    #[serde(default)]
    pub requires_js: bool,
    pub js_source: Option<String>,
    /// Set by the converter when a generator was previously mis-converted and has been
    /// corrected in the named release. Used by `ghost-complete doctor` to surface
    /// generators that silently changed behaviour.
    #[serde(default, rename = "_corrected_in")]
    pub corrected_in: Option<String>,
    /// Fig-compatible template field on generators (e.g., "filepaths", "folders",
    /// or ["filepaths", "folders"]). Treated the same as `ArgSpec.template`.
    #[serde(default, deserialize_with = "deserialize_template")]
    pub template: Option<String>,
}

pub struct SpecStore {
    specs: HashMap<String, CompletionSpec>,
}

pub struct SpecLoadResult {
    pub store: SpecStore,
    pub errors: Vec<String>,
}

impl SpecStore {
    /// Load specs from multiple directories with first-match-wins merging:
    /// a spec from an earlier directory is not overridden by a later one.
    /// This matches the user intuition that earlier entries in config's
    /// `paths.spec_dirs` take precedence (e.g., user overrides before
    /// system defaults).
    pub fn load_from_dirs(dirs: &[PathBuf]) -> Result<SpecLoadResult> {
        let mut specs: HashMap<String, CompletionSpec> = HashMap::new();
        let mut errors: Vec<String> = Vec::new();
        for dir in dirs {
            match Self::load_from_dir(dir) {
                Ok(result) => {
                    for (name, spec) in result.store.specs {
                        specs.entry(name).or_insert(spec);
                    }
                    errors.extend(result.errors);
                }
                Err(e) => {
                    // Directory-level IO failure (e.g., EACCES on read_dir).
                    // Accumulate into errors like per-file failures instead
                    // of bailing — a broken dir earlier in the list must not
                    // hide valid dirs later in the list. Symmetric with
                    // load_from_dir's per-file error handling.
                    errors.push(format!("{}: {e}", dir.display()));
                }
            }
        }
        Ok(SpecLoadResult {
            store: Self { specs },
            errors,
        })
    }

    pub fn load_from_dir(dir: &Path) -> Result<SpecLoadResult> {
        let mut specs = HashMap::new();
        let mut errors = Vec::new();

        if !dir.exists() {
            tracing::warn!("spec directory does not exist: {}", dir.display());
            return Ok(SpecLoadResult {
                store: Self { specs },
                errors,
            });
        }

        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read spec directory: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            match Self::load_spec(&path) {
                Ok(spec) => {
                    tracing::debug!("loaded spec: {}", spec.name);
                    specs.insert(spec.name.clone(), spec);
                }
                Err(e) => {
                    errors.push(format!("{file_name}: {e}"));
                }
            }
        }

        Ok(SpecLoadResult {
            store: Self { specs },
            errors,
        })
    }

    fn load_spec(path: &Path) -> Result<CompletionSpec> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read spec file: {}", path.display()))?;
        let mut spec = parse_spec_checked_and_sanitized(&contents)
            .with_context(|| format!("failed to parse spec file: {}", path.display()))?;
        let warnings = validate_spec_generators(&mut spec);
        for w in &warnings {
            tracing::warn!("{}: {w}", spec.name);
        }
        Ok(spec)
    }

    pub fn get(&self, command: &str) -> Option<&CompletionSpec> {
        self.specs.get(command)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &CompletionSpec)> {
        self.specs.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

/// Shared entry point for parsing spec JSON. Enforces the nesting-depth cap
/// BEFORE invoking `serde_json::from_str` (so attacker-crafted input cannot
/// blow the stack inside the parser), then strips control characters from
/// every user-facing string. Any caller that hands raw on-disk bytes to the
/// completion pipeline must go through this function — skipping it
/// re-introduces the CVE class this cap was added to prevent.
pub fn parse_spec_checked_and_sanitized(contents: &str) -> Result<CompletionSpec> {
    check_json_depth(contents, MAX_SPEC_JSON_DEPTH)?;
    let mut spec: CompletionSpec = serde_json::from_str(contents)?;
    sanitize_spec_strings(&mut spec);
    Ok(spec)
}

pub struct SpecResolution {
    pub subcommands: Vec<Suggestion>,
    pub options: Vec<Suggestion>,
    pub native_generators: Vec<String>,
    /// Phase 3A native providers resolved from the spec (e.g.
    /// `cargo_targets`). The engine dispatches these asynchronously
    /// via `resolve_providers`. Parallel to `native_generators` — we
    /// translate the `"type"` string into `ProviderKind` at spec
    /// resolution time so the engine does not re-parse strings on the
    /// keystroke hot path. See `providers::kind_from_type_str`.
    pub provider_generators: Vec<ProviderKind>,
    /// `Arc<GeneratorSpec>` rather than `GeneratorSpec`: `collect_generators`
    /// and the downstream `handler::spawn_generators` copy this vec on the
    /// hot path (every resolution + every async spawn). Arc'ing makes each
    /// clone a refcount bump instead of a deep copy of `Vec<Transform>`,
    /// `Vec<String>` argv, and `Option<CacheConfig>`.
    pub script_generators: Vec<Arc<GeneratorSpec>>,
    pub wants_filepaths: bool,
    pub wants_folders_only: bool,
    /// True when the preceding flag's own `args` spec contributed generators
    /// or templates. Used by `engine.rs` to suppress subcommands/options when
    /// the user is filling in a flag's argument (e.g. `curl -o <TAB>`).
    /// False when the preceding flag is boolean (no args) — positional-arg
    /// generators should NOT suppress subcommands/options in that case.
    pub preceding_flag_has_args: bool,
    /// True when a `--` (end-of-flags) separator was seen in the args before
    /// the current position. After `--`, all tokens are positional — the
    /// engine should suppress both subcommands and options.
    pub past_double_dash: bool,
}

/// Walk the spec tree using args from the CommandContext to find the deepest
/// matching subcommand, then return available completions at that position.
pub fn resolve_spec(spec: &CompletionSpec, ctx: &CommandContext) -> SpecResolution {
    // Start at the top-level spec
    let mut current_subcommands = &spec.subcommands;
    let mut current_options = &spec.options;
    let mut current_args = &spec.args;

    // Walk through ctx.args, greedily matching subcommand names.
    // Once a non-flag, non-subcommand token is encountered (a positional
    // arg), stop subcommand matching — subsequent tokens are positional
    // even if they happen to match a subcommand name. Without this guard,
    // `git push.sh push` would incorrectly match `push` as a subcommand
    // after the positional `push.sh`.
    let mut arg_idx = 0;
    let mut past_positional = false;
    let args = &ctx.args;

    while arg_idx < args.len() {
        let arg = &args[arg_idx];

        // `--` marks end of flags — all subsequent tokens are positional
        if arg == "--" {
            past_positional = true;
            arg_idx += 1;
            continue;
        }

        // Skip flags
        if arg.starts_with('-') {
            // If this flag takes a value in the spec, skip the next arg too
            // (unless the value is inline via `--flag=value`, where there's
            // no separate next arg to skip).
            if let Some(opt) = find_option(current_options, arg) {
                if opt.args.is_some() && !arg.contains('=') && arg_idx + 1 < args.len() {
                    arg_idx += 2;
                    continue;
                }
            }
            arg_idx += 1;
            continue;
        }

        // Try to match a subcommand (only before the first positional arg)
        if !past_positional {
            if let Some(sub) = current_subcommands.iter().find(|s| s.name == *arg) {
                current_subcommands = &sub.subcommands;
                current_options = &sub.options;
                current_args = &sub.args;
                arg_idx += 1;
                continue;
            }
        }

        // Positional argument — all subsequent non-flag tokens are
        // positional too.
        past_positional = true;
        arg_idx += 1;
    }

    // Build suggestions from the resolved position
    let subcommand_suggestions: Vec<Suggestion> = current_subcommands
        .iter()
        .map(|s| Suggestion {
            text: s.name.clone(),
            description: s.description.clone(),
            kind: SuggestionKind::Subcommand,
            source: SuggestionSource::Spec,
            ..Default::default()
        })
        .collect();

    let option_suggestions: Vec<Suggestion> = current_options
        .iter()
        .flat_map(|o| {
            o.name.iter().map(move |n| Suggestion {
                text: n.clone(),
                description: o.description.clone(),
                kind: SuggestionKind::Flag,
                source: SuggestionSource::Spec,
                ..Default::default()
            })
        })
        .collect();

    // Collect generator types from args at the resolved position
    let mut native_generators = Vec::new();
    let mut provider_generators = Vec::new();
    let mut script_generators = Vec::new();
    let mut wants_filepaths = false;
    let mut wants_folders_only = false;

    // If the preceding token was a flag that takes an argument, check
    // the option's arg spec for templates/generators instead of the
    // positional args.
    let mut preceding_flag_has_args = false;
    if let Some(flag) = &ctx.preceding_flag {
        if let Some(opt) = find_option(current_options, flag) {
            if let Some(arg_spec) = &opt.args {
                // The flag takes an argument — suppress subcommands/options
                // regardless of whether the arg spec has explicit generators.
                // A bare `"args": { "name": "file" }` still means the user
                // is filling a value, not typing a subcommand.
                preceding_flag_has_args = true;

                collect_generators(
                    &arg_spec.generators,
                    &mut native_generators,
                    &mut provider_generators,
                    &mut script_generators,
                    &mut wants_filepaths,
                    &mut wants_folders_only,
                );
                match arg_spec.template.as_deref() {
                    Some("filepaths") => wants_filepaths = true,
                    Some("folders") => wants_folders_only = true,
                    _ => {}
                }
            }
        }
    }

    // Also check positional arg specs at the resolved position
    for arg_spec in current_args {
        collect_generators(
            &arg_spec.generators,
            &mut native_generators,
            &mut provider_generators,
            &mut script_generators,
            &mut wants_filepaths,
            &mut wants_folders_only,
        );
        match arg_spec.template.as_deref() {
            Some("filepaths") => wants_filepaths = true,
            Some("folders") => wants_folders_only = true,
            _ => {}
        }
    }

    SpecResolution {
        subcommands: subcommand_suggestions,
        options: option_suggestions,
        native_generators,
        provider_generators,
        script_generators,
        wants_filepaths,
        wants_folders_only,
        preceding_flag_has_args,
        past_double_dash: past_positional && ctx.args.iter().any(|a| a == "--"),
    }
}

fn collect_generators(
    generators: &[GeneratorSpec],
    native: &mut Vec<String>,
    provider: &mut Vec<ProviderKind>,
    script: &mut Vec<Arc<GeneratorSpec>>,
    wants_filepaths: &mut bool,
    wants_folders_only: &mut bool,
) {
    for gen in generators {
        if gen.requires_js {
            tracing::info!("skipping generator requiring JS runtime");
            continue;
        }
        // Three-way dispatch on `generator_type`, with script fall-through
        // ONLY on the unknown-type path. A generator that names a registered
        // provider or a known native type wins outright — the script block
        // is intentionally skipped so a spec with both `type` and `script`
        // does not double-dispatch (native/provider result set + script
        // result set merged together). Our converter never emits both, but
        // Phase 3A T2-T9 provider tests need to assert "provider path wins"
        // cleanly, which requires exactly this guard.
        let handled_by_type = if let Some(ref gen_type) = gen.generator_type {
            if let Some(kind) = providers::kind_from_type_str(gen_type) {
                // Phase 3A native provider — routed to the async
                // provider pipeline instead of the legacy native/script
                // paths. The provider IS the implementation; do not
                // also push onto `native` or fall through to the script
                // branch below.
                provider.push(kind);
                true
            } else if KNOWN_NATIVE_GENERATOR_TYPES.contains(&gen_type.as_str()) {
                native.push(gen_type.clone());
                true
            } else {
                // Unknown type — preserve previous behavior (still push
                // to `native` so downstream code paths are unchanged,
                // and surface a warning so misconfigured specs don't
                // silently produce zero completions). We deliberately
                // DO fall through to the script branch: a spec that
                // pairs an unrecognized type string with a real
                // `script` block should still run the script, matching
                // pre-Phase-3A behavior.
                tracing::warn!(
                    generator_type = %gen_type,
                    "unknown generator type — no completions will be produced"
                );
                native.push(gen_type.clone());
                false
            }
        } else {
            false
        };
        if !handled_by_type && (gen.script.is_some() || gen.script_template.is_some()) {
            script.push(Arc::new(gen.clone()));
        }
        // Fig specs put template on generators too (e.g., git checkout's
        // `{"template": ["filepaths", "folders"]}`).
        match gen.template.as_deref() {
            Some("filepaths") => *wants_filepaths = true,
            Some("folders") => *wants_folders_only = true,
            _ => {}
        }
    }
}

/// Linear-scan option lookup. Previously guarded a `HashMap`-backed
/// `OptionsIndex`; the eager-build pattern lost to linear scan in every
/// realistic `resolve_spec` call (benchmarks regressed 40–62% with the
/// index) because a typical shell command line performs 0–3 flag lookups
/// while each subcommand descent rebuilt the map. For 200-option specs the
/// linear scan is still sub-microsecond — the crossover where a HashMap
/// would pay off is far outside any real command line.
fn find_option<'a>(options: &'a [OptionSpec], flag: &str) -> Option<&'a OptionSpec> {
    // Strip `=value` suffix so `--flag=value` matches an option named `--flag`.
    let base_flag = flag.split_once('=').map_or(flag, |(base, _)| base);
    options
        .iter()
        .find(|o| o.name.iter().any(|n| n == base_flag))
}

/// Walk all generators in a spec tree, validate their transform pipelines,
/// and remove generators with invalid pipelines. Returns warnings for each
/// removed generator.
///
/// Iterative on purpose: a deeply nested attacker-supplied spec must not be
/// able to stack-overflow this walker even if it slips past the depth cap.
pub fn validate_spec_generators(spec: &mut CompletionSpec) -> Vec<String> {
    let mut warnings = Vec::new();
    validate_args_generators(&mut spec.args, &spec.name, &mut warnings);
    for opt in &mut spec.options {
        if let Some(ref mut arg_spec) = opt.args {
            validate_arg_generators(arg_spec, &spec.name, &mut warnings);
        }
    }

    let mut stack: Vec<&mut SubcommandSpec> = spec.subcommands.iter_mut().collect();
    while let Some(sub) = stack.pop() {
        validate_args_generators(&mut sub.args, &spec.name, &mut warnings);
        for opt in &mut sub.options {
            if let Some(ref mut arg_spec) = opt.args {
                validate_arg_generators(arg_spec, &spec.name, &mut warnings);
            }
        }
        stack.extend(sub.subcommands.iter_mut());
    }

    warnings
}

fn validate_args_generators(args: &mut [ArgSpec], spec_name: &str, warnings: &mut Vec<String>) {
    for arg_spec in args.iter_mut() {
        validate_arg_generators(arg_spec, spec_name, warnings);
    }
}

fn validate_arg_generators(arg_spec: &mut ArgSpec, spec_name: &str, warnings: &mut Vec<String>) {
    use crate::transform::validate_pipeline;

    let original_len = arg_spec.generators.len();
    arg_spec.generators.retain(|gen| {
        if gen.transforms.is_empty() {
            return true;
        }
        match validate_pipeline(&gen.transforms) {
            Ok(()) => true,
            Err(e) => {
                warnings.push(format!(
                    "generator in {spec_name} has invalid transform pipeline: {e}"
                ));
                false
            }
        }
    });
    if arg_spec.generators.len() < original_len {
        tracing::warn!(
            "{spec_name}: removed {} generator(s) with invalid transform pipelines",
            original_len - arg_spec.generators.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_spec() -> CompletionSpec {
        serde_json::from_str(
            r#"{
                "name": "test-cmd",
                "description": "A test command",
                "subcommands": [
                    {
                        "name": "sub1",
                        "description": "First subcommand",
                        "options": [
                            { "name": ["--verbose", "-v"], "description": "Verbose output" }
                        ],
                        "args": [
                            {
                                "name": "target",
                                "generators": [{ "type": "git_branches" }]
                            }
                        ]
                    },
                    {
                        "name": "sub2",
                        "description": "Second subcommand"
                    }
                ],
                "options": [
                    { "name": ["--help", "-h"], "description": "Show help" }
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn test_curl_dash_o_resolve_spec_sets_wants_filepaths() {
        let spec_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/curl.json");
        if !spec_path.exists() {
            eprintln!("curl.json not found, skipping");
            return;
        }
        let contents = std::fs::read_to_string(&spec_path).unwrap();
        let spec: CompletionSpec = serde_json::from_str(&contents).unwrap();

        // curl -o <TAB>
        let ctx = CommandContext {
            command: Some("curl".into()),
            args: vec!["-o".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-o".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        eprintln!(
            "wants_filepaths={}, wants_folders_only={}, generators={:?}",
            res.wants_filepaths, res.wants_folders_only, res.native_generators
        );
        assert!(
            res.wants_filepaths,
            "curl -o should set wants_filepaths from the -o option's args template"
        );
    }

    #[test]
    fn test_deserialize_git_spec() {
        let spec_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/git.json");
        if spec_path.exists() {
            let contents = std::fs::read_to_string(&spec_path).unwrap();
            let spec: CompletionSpec = serde_json::from_str(&contents).unwrap();
            assert_eq!(spec.name, "git");
            assert!(!spec.subcommands.is_empty());
        }
    }

    #[test]
    fn test_resolve_top_level_subcommands() {
        let spec = test_spec();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        let names: Vec<&str> = res.subcommands.iter().map(|s| s.text.as_str()).collect();
        assert!(names.contains(&"sub1"));
        assert!(names.contains(&"sub2"));
    }

    #[test]
    fn test_resolve_subcommand_options() {
        let spec = test_spec();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec!["sub1".into()],
            current_word: "--".into(),
            word_index: 2,
            is_flag: true,
            is_long_flag: true,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        let names: Vec<&str> = res.options.iter().map(|s| s.text.as_str()).collect();
        assert!(names.contains(&"--verbose"));
        assert!(names.contains(&"-v"));
    }

    #[test]
    fn test_resolve_generators() {
        let spec = test_spec();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec!["sub1".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(res.native_generators.contains(&"git_branches".to_string()));
    }

    #[test]
    fn test_resolve_unknown_subcommand_doesnt_panic() {
        let spec = test_spec();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec!["nonexistent".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        // Should not panic — returns top-level completions since "nonexistent"
        // didn't match any subcommand
        assert!(res.subcommands.is_empty() || !res.subcommands.is_empty());
    }

    #[test]
    fn test_folders_template_sets_wants_folders_only() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "cd",
                "description": "Change directory",
                "args": [{ "name": "directory", "template": "folders" }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("cd".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.wants_folders_only,
            "folders template should set wants_folders_only"
        );
        assert!(
            !res.wants_filepaths,
            "folders template should NOT set wants_filepaths"
        );
    }

    #[test]
    fn test_filepaths_template_sets_wants_filepaths() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "cat",
                "description": "Concatenate files",
                "args": [{ "name": "file", "template": "filepaths" }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("cat".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.wants_filepaths,
            "filepaths template should set wants_filepaths"
        );
        assert!(
            !res.wants_folders_only,
            "filepaths template should NOT set wants_folders_only"
        );
    }

    #[test]
    fn test_option_arg_filepaths_template_via_preceding_flag() {
        // pip install -r <TAB> → should want filepaths
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "pip",
                "description": "Python package installer",
                "subcommands": [{
                    "name": "install",
                    "description": "Install packages",
                    "options": [{
                        "name": ["-r", "--requirement"],
                        "description": "Install from requirements file",
                        "args": { "name": "file", "template": "filepaths" }
                    }]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("pip".into()),
            args: vec!["install".into(), "-r".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-r".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.wants_filepaths,
            "option with filepaths template should set wants_filepaths when preceding_flag matches"
        );
    }

    #[test]
    fn test_option_arg_folders_template_via_preceding_flag() {
        // pip install -t <TAB> → should want folders only
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "pip",
                "description": "Python package installer",
                "subcommands": [{
                    "name": "install",
                    "description": "Install packages",
                    "options": [{
                        "name": ["-t", "--target"],
                        "description": "Install into this directory",
                        "args": { "name": "dir", "template": "folders" }
                    }]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("pip".into()),
            args: vec!["install".into(), "-t".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-t".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.wants_folders_only,
            "option with folders template should set wants_folders_only when preceding_flag matches"
        );
        assert!(
            !res.wants_filepaths,
            "folders template should NOT set wants_filepaths"
        );
    }

    #[test]
    fn test_option_arg_generator_via_preceding_flag() {
        // git checkout -b <TAB> with a generator on the option
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "test-cmd",
                "description": "Test",
                "options": [{
                    "name": ["-b", "--branch"],
                    "description": "Branch name",
                    "args": {
                        "name": "branch",
                        "generators": [{ "type": "git_branches" }]
                    }
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec!["-b".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-b".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.native_generators.contains(&"git_branches".to_string()),
            "option arg generators should be collected via preceding_flag"
        );
    }

    #[test]
    fn test_no_preceding_flag_no_option_template() {
        // pip install <TAB> without a preceding flag — should NOT trigger option templates
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "pip",
                "description": "Python package installer",
                "subcommands": [{
                    "name": "install",
                    "description": "Install packages",
                    "options": [{
                        "name": ["-r"],
                        "description": "Requirements file",
                        "args": { "name": "file", "template": "filepaths" }
                    }]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("pip".into()),
            args: vec!["install".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            !res.wants_filepaths,
            "should NOT want filepaths when no preceding_flag is set"
        );
    }

    #[test]
    fn test_load_from_dir_mixed_valid_and_invalid() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("good.json"),
            r#"{"name": "good", "args": []}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("bad.json"), r#"{"not_a_spec": true}"#).unwrap();
        std::fs::write(dir.path().join("broken.json"), "{ totally busted").unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        assert!(
            result.store.get("good").is_some(),
            "valid spec should be loaded"
        );
        assert_eq!(result.errors.len(), 2, "should have 2 errors");
        assert!(
            result.errors.iter().any(|e| e.starts_with("bad.json:")),
            "errors should include bad.json: {:?}",
            result.errors
        );
        assert!(
            result.errors.iter().any(|e| e.starts_with("broken.json:")),
            "errors should include broken.json: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_load_from_dir_all_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("alpha.json"), r#"{"name": "alpha"}"#).unwrap();
        std::fs::write(dir.path().join("beta.json"), r#"{"name": "beta"}"#).unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        assert!(result.errors.is_empty(), "no errors expected");
        assert!(result.store.get("alpha").is_some());
        assert!(result.store.get("beta").is_some());
    }

    #[test]
    fn test_load_from_dir_nonexistent() {
        let result = SpecStore::load_from_dir(Path::new("/nonexistent/path/specs")).unwrap();
        assert!(result.errors.is_empty());
        assert!(result.store.get("anything").is_none());
    }

    #[test]
    fn test_deserialize_native_generator() {
        let gen: GeneratorSpec = serde_json::from_str(r#"{"type": "git_branches"}"#).unwrap();
        assert_eq!(gen.generator_type.as_deref(), Some("git_branches"));
        assert!(gen.script.is_none());
        assert!(gen.script_template.is_none());
        assert!(gen.transforms.is_empty());
        assert!(gen.cache.is_none());
        assert!(!gen.requires_js);
        assert!(gen.js_source.is_none());
    }

    #[test]
    fn test_deserialize_script_generator() {
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{"script": ["brew", "formulae"], "cache": {"ttl_seconds": 300}}"#,
        )
        .unwrap();
        assert!(gen.generator_type.is_none());
        assert_eq!(
            gen.script.as_deref(),
            Some(&["brew".to_string(), "formulae".to_string()][..])
        );
        assert!(gen.script_template.is_none());
        assert!(gen.transforms.is_empty());
        let cache = gen.cache.unwrap();
        assert_eq!(cache.ttl_seconds, 300);
        assert!(!cache.cache_by_directory);
    }

    #[test]
    fn test_deserialize_script_generator_with_transforms() {
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{
                "script": ["brew", "formulae"],
                "transforms": ["split_lines", "filter_empty", "trim"],
                "cache": {"ttl_seconds": 300}
            }"#,
        )
        .unwrap();
        assert_eq!(gen.transforms.len(), 3);
    }

    #[test]
    fn test_deserialize_script_template_generator() {
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{"script_template": ["cmd", "{prev_token}"], "transforms": ["split_lines"]}"#,
        )
        .unwrap();
        assert!(gen.generator_type.is_none());
        assert!(gen.script.is_none());
        assert_eq!(
            gen.script_template.as_deref(),
            Some(&["cmd".to_string(), "{prev_token}".to_string()][..])
        );
        assert_eq!(gen.transforms.len(), 1);
    }

    #[test]
    fn test_deserialize_requires_js_generator() {
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{"requires_js": true, "js_source": "module.exports = { ... }"}"#,
        )
        .unwrap();
        assert!(gen.requires_js);
        assert_eq!(gen.js_source.as_deref(), Some("module.exports = { ... }"));
    }

    #[test]
    fn test_deserialize_corrected_in_generator() {
        // The converter emits `_corrected_in` on generators that were
        // previously mis-converted. Verify it round-trips through the
        // `#[serde(rename = "_corrected_in")]` field.
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{"requires_js": true, "js_source": "fn body", "_corrected_in": "v0.10.0"}"#,
        )
        .unwrap();
        assert!(gen.requires_js);
        assert_eq!(gen.corrected_in.as_deref(), Some("v0.10.0"));
    }

    #[test]
    fn test_deserialize_corrected_in_defaults_to_none() {
        // Generators that were correctly converted have no `_corrected_in`
        // field. Ensure the default is None so every existing spec parses.
        let gen: GeneratorSpec = serde_json::from_str(r#"{"type": "git_branches"}"#).unwrap();
        assert!(gen.corrected_in.is_none());
    }

    #[test]
    fn test_resolve_spec_splits_generators() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "test-mixed",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "git_branches"},
                        {"script": ["some-cmd"], "transforms": ["split_lines"]},
                        {"requires_js": true, "js_source": "..."}
                    ]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("test-mixed".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert_eq!(res.native_generators, vec!["git_branches"]);
        assert_eq!(res.script_generators.len(), 1);
        assert!(res.script_generators[0].script.is_some());
    }

    #[test]
    fn test_validate_spec_strips_invalid_generator_pipeline() {
        // A spec with one valid generator and one with an invalid pipeline
        // (post-split transform before split). The invalid one should be
        // stripped during load_spec.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "git_branches"},
                        {"script": ["cmd"], "transforms": ["filter_empty", "split_lines"]}
                    ]
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        // The second generator should have been removed
        assert_eq!(
            spec.args[0].generators.len(),
            1,
            "invalid generator should be removed; remaining: {:?}",
            spec.args[0].generators
        );
        assert_eq!(
            spec.args[0].generators[0].generator_type.as_deref(),
            Some("git_branches"),
        );
    }

    #[test]
    fn test_validate_spec_keeps_valid_pipeline() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"script": ["cmd"], "transforms": ["split_lines", "filter_empty", "trim"]}
                    ]
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        assert_eq!(
            spec.args[0].generators.len(),
            1,
            "valid generator should be kept"
        );
    }

    #[test]
    fn test_validate_spec_empty_transforms_kept() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "git_branches"},
                        {"script": ["cmd"]}
                    ]
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        assert_eq!(
            spec.args[0].generators.len(),
            2,
            "generators with empty transforms should be kept"
        );
    }

    #[test]
    fn test_validate_spec_recursive_subcommands() {
        // Ensure validation walks into nested subcommands
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "subcommands": [{
                    "name": "sub",
                    "args": [{
                        "name": "target",
                        "generators": [
                            {"script": ["cmd"], "transforms": ["trim", "split_lines"]}
                        ]
                    }]
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        assert_eq!(
            spec.subcommands[0].args[0].generators.len(),
            0,
            "invalid generator in subcommand should be removed"
        );
    }

    #[test]
    fn test_find_option_with_equals_value() {
        let options = vec![OptionSpec {
            name: vec!["--output".into(), "-o".into()],
            description: Some("Output format".into()),
            args: Some(ArgSpec {
                name: Some("format".into()),
                description: None,
                generators: vec![],
                template: None,
                suggestions: None,
            }),
        }];
        // Exact match
        assert!(find_option(&options, "--output").is_some());
        // With =value suffix
        assert!(find_option(&options, "--output=json").is_some());
        // Short flag still works
        assert!(find_option(&options, "-o").is_some());
        // Non-existent
        assert!(find_option(&options, "--format").is_none());
    }

    #[test]
    fn test_find_option_handles_large_spec_with_equals_value() {
        // 200 options × 2 aliases each. Every alias must resolve correctly —
        // including `--flag=value` and the unknown-alias case.
        let mut options: Vec<OptionSpec> = Vec::with_capacity(200);
        for i in 0..200 {
            options.push(OptionSpec {
                name: vec![format!("--opt-{i}"), format!("-o{i}")],
                description: Some(format!("option {i}")),
                args: if i % 2 == 0 {
                    Some(ArgSpec {
                        name: Some("val".into()),
                        description: None,
                        generators: vec![],
                        template: None,
                        suggestions: None,
                    })
                } else {
                    None
                },
            });
        }

        for i in 0..200 {
            let long = format!("--opt-{i}");
            let short = format!("-o{i}");
            let eq = format!("--opt-{i}=value");
            assert_eq!(
                find_option(&options, &long).map(|o| &o.name[0]),
                Some(&long),
            );
            assert_eq!(
                find_option(&options, &short).map(|o| &o.name[0]),
                Some(&long)
            );
            assert_eq!(find_option(&options, &eq).map(|o| &o.name[0]), Some(&long));
        }

        assert!(find_option(&options, "--nope").is_none());
    }

    #[test]
    fn test_validate_spec_option_args() {
        // Ensure validation walks into option arg specs
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "options": [{
                    "name": ["-f"],
                    "description": "flag",
                    "args": {
                        "name": "val",
                        "generators": [
                            {"script": ["cmd"], "transforms": ["split_lines", "split_lines"]}
                        ]
                    }
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        assert_eq!(
            spec.options[0].args.as_ref().unwrap().generators.len(),
            0,
            "double-split generator in option args should be removed"
        );
    }

    /// Build a JSON string with `depth` levels of `subcommands` nesting and
    /// return it. The structure is:
    ///   { "name": "x", "subcommands": [ { "name": "x", "subcommands": [ ... ] } ] }
    fn build_nested_subcommands(depth: usize) -> String {
        let mut s = String::with_capacity(depth * 32);
        for _ in 0..depth {
            s.push_str(r#"{"name":"x","subcommands":["#);
        }
        s.push_str(r#"{"name":"leaf"}"#);
        for _ in 0..depth {
            s.push_str("]}");
        }
        s
    }

    #[test]
    fn test_load_spec_rejects_pathologically_nested_json() {
        // Attacker-writable spec with 10k nested subcommands must be rejected
        // at parse time, before any spec walker runs. Without a depth cap this
        // overflows the stack on serde_json's recursive parser.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("evil.json"),
            build_nested_subcommands(10_000),
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        assert!(
            result.store.get("x").is_none() && result.store.get("leaf").is_none(),
            "pathologically nested spec must not load"
        );
        assert_eq!(result.errors.len(), 1, "expected one load error");
        assert!(
            result.errors[0].contains("evil.json"),
            "error should reference evil.json: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_load_spec_rejects_moderately_nested_json_above_cap() {
        // Real-world max is 7; the cap is 32. A depth-100 spec is well below
        // serde_json's default 128 recursion limit but well above our cap, so
        // it must be rejected by our own preflight depth check before it can
        // exercise our recursive walkers.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("evil.json"), build_nested_subcommands(100)).unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        assert!(
            result.store.get("x").is_none() && result.store.get("leaf").is_none(),
            "depth-100 spec must be rejected by our own cap (serde_json's 128 default would still let it through)"
        );
        assert_eq!(result.errors.len(), 1, "expected one load error");
    }

    #[test]
    fn test_load_spec_accepts_real_world_depth() {
        // The deepest real-world spec (atlas.json) has subcommand depth ~7
        // (each subcommand adds two JSON levels: `{"subcommands":[`). The
        // 12-deep fixture below corresponds to 24 JSON levels + the leaf —
        // well within the 32-level cap and well above any real spec.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("ok.json"), build_nested_subcommands(12)).unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        assert!(
            result.errors.is_empty(),
            "depth-12 spec should parse cleanly, got errors: {:?}",
            result.errors
        );
        assert!(
            result.store.get("x").is_some(),
            "depth-12 spec should be loaded"
        );
    }

    #[test]
    fn test_load_spec_strips_ansi_from_name_and_description() {
        // Malicious spec with CSI/OSC sequences in name and description must
        // be sanitized at load time so a downstream renderer cannot be tricked
        // into emitting an injected escape sequence.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("evil.json"),
            "{\
                \"name\": \"evil\\u001b[2J\",\
                \"description\": \"steal\\u001b]0;pwned\\u0007rest\",\
                \"subcommands\": [\
                    {\"name\": \"sub\\u001b[2J\", \"description\": \"d\\u001bx\"}\
                ],\
                \"options\": [\
                    {\"name\": [\"--flag\"], \"description\": \"o\\u001b[2J\"}\
                ]\
            }",
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result
            .store
            .get("evil[2J")
            .expect("spec should load with sanitized name");
        assert!(
            !spec.name.contains('\x1b'),
            "name kept ESC: {:?}",
            spec.name
        );
        assert!(
            !spec.description.as_deref().unwrap_or("").contains('\x1b'),
            "description kept ESC: {:?}",
            spec.description
        );
        assert!(
            !spec.description.as_deref().unwrap_or("").contains('\x07'),
            "description kept BEL: {:?}",
            spec.description
        );
        assert!(
            !spec.subcommands[0].name.contains('\x1b'),
            "subcommand name kept ESC: {:?}",
            spec.subcommands[0].name
        );
        assert!(
            !spec.subcommands[0]
                .description
                .as_deref()
                .unwrap_or("")
                .contains('\x1b'),
            "subcommand description kept ESC"
        );
        assert!(
            !spec.options[0]
                .description
                .as_deref()
                .unwrap_or("")
                .contains('\x1b'),
            "option description kept ESC"
        );
    }

    #[test]
    fn test_check_json_depth_accepts_well_within_cap() {
        let src = build_nested_subcommands(7);
        assert!(check_json_depth(&src, MAX_SPEC_JSON_DEPTH).is_ok());
    }

    #[test]
    fn test_check_json_depth_ignores_brackets_inside_strings() {
        // A string literal full of `{` must not contribute to the depth count.
        let src = format!(r#"{{"name":"{}"}}"#, "{".repeat(1000));
        assert!(check_json_depth(&src, 4).is_ok());
    }

    #[test]
    fn test_validate_spec_generators_iterative_handles_deep_subcommand_chain() {
        // Even a depth-200 chain (which could blow the stack on the old
        // recursive walker) must run without overflowing because the new
        // implementation is iterative.
        let mut spec = CompletionSpec {
            name: "deep".into(),
            description: None,
            subcommands: Vec::new(),
            options: Vec::new(),
            args: Vec::new(),
        };
        let mut tail = &mut spec.subcommands;
        for i in 0..200 {
            tail.push(SubcommandSpec {
                name: format!("s{i}"),
                description: None,
                subcommands: Vec::new(),
                options: Vec::new(),
                args: Vec::new(),
            });
            tail = &mut tail[0].subcommands;
        }
        // Should not panic / stack-overflow
        let warnings = validate_spec_generators(&mut spec);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_resolve_spec_provider_generators_empty_by_default() {
        // Specs that use only git/filepath generators must leave
        // `provider_generators` empty. Locks in that the scaffolding
        // does not accidentally route existing native types into the
        // Phase 3A pipeline.
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "test-no-providers",
                "args": [{
                    "name": "target",
                    "generators": [{"type": "git_branches"}],
                    "template": "filepaths"
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("test-no-providers".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.provider_generators.is_empty(),
            "expected empty provider_generators for non-provider spec: {:?}",
            res.provider_generators
        );
    }

    #[test]
    fn test_resolve_spec_known_type_plus_script_does_not_double_dispatch() {
        // A GeneratorSpec with BOTH a recognized `type` and a `script`
        // must dispatch ONLY to the native/provider path, never also to
        // the script pipeline. Otherwise a spec carrying a type string
        // alongside a script body (today hypothetical for the git types,
        // tomorrow a real concern for Phase 3A providers) would merge
        // two result sets into the same popup.
        //
        // Uses `git_branches` — the empty `ProviderKind` at T1 means we
        // cannot exercise the provider arm here; the native arm exercises
        // the same `handled_by_type` guard, so the invariant is covered.
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "test-dual-dispatch",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "git_branches", "script": ["echo", "should-not-run"]}
                    ]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("test-dual-dispatch".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.native_generators.contains(&"git_branches".to_string()),
            "native arm must win when `type` is a known native"
        );
        assert!(
            res.script_generators.is_empty(),
            "script must NOT also dispatch when `type` matched a native arm: got {:?}",
            res.script_generators
        );
    }

    #[test]
    fn test_resolve_spec_unknown_type_plus_script_still_dispatches_script() {
        // Complement to the double-dispatch test above: when `type` is
        // an unrecognized string (unknown-type warn path), the script
        // block MUST still dispatch. This preserves pre-Phase-3A
        // behavior — specs that paired a junk type string with a real
        // script were relying on the script to run.
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "test-unknown-plus-script",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "nonexistent_provider", "script": ["echo", "ok"]}
                    ]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("test-unknown-plus-script".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.provider_generators.is_empty(),
            "unknown type must not route to providers"
        );
        assert_eq!(
            res.script_generators.len(),
            1,
            "script must still dispatch on unknown-type + script combo"
        );
    }

    #[test]
    fn test_resolve_spec_unknown_provider_type_does_not_route_to_providers() {
        // A spec that names a `type` we have not registered (and which
        // is not in `KNOWN_NATIVE_GENERATOR_TYPES`) must NOT end up in
        // `provider_generators`. The existing unknown-type warn path
        // still owns that string — falls through to `native_generators`
        // so downstream behavior is unchanged at T1.
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "test-unknown-provider",
                "args": [{
                    "name": "target",
                    "generators": [{"type": "nonexistent_provider"}]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("test-unknown-provider".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.provider_generators.is_empty(),
            "unknown generator_type must not be routed to provider_generators"
        );
        assert!(
            res.native_generators
                .contains(&"nonexistent_provider".to_string()),
            "unknown generator_type should still land in native_generators (preserves unknown-type warn path)"
        );
    }
}
