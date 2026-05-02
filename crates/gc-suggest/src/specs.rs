use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::priority::Priority;
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

fn sanitize_suggestion_object(obj: &mut SuggestionObject) {
    sanitize_opt(&mut obj.description);
    for n in &mut obj.name {
        sanitize_string(n);
    }
}

fn sanitize_arg_spec(arg: &mut ArgSpec) {
    sanitize_opt(&mut arg.name);
    sanitize_opt(&mut arg.description);
    for entry in &mut arg.suggestions {
        match entry {
            SuggestionEntry::Plain(s) => sanitize_string(s),
            SuggestionEntry::Object(obj) => sanitize_suggestion_object(obj),
        }
    }
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
    #[serde(default)]
    pub priority: Option<Priority>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OptionSpec {
    pub name: Vec<String>,
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_option_args")]
    pub args: Option<ArgSpec>,
    #[serde(default)]
    pub priority: Option<Priority>,
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
    /// Static suggestions — plain string or full object entries from the spec's
    /// `args.suggestions` field.
    ///
    /// `pub(crate)` because `SuggestionEntry` itself is `pub(crate)` —
    /// external consumers (`ghost-complete::status`/`doctor`) only inspect
    /// `args` and `generators`, never `suggestions`.
    #[serde(default, deserialize_with = "deserialize_suggestions_one_or_many")]
    pub(crate) suggestions: Vec<SuggestionEntry>,
}

/// Static suggestion entry — either a plain string shorthand or a full object.
/// Mirrors the Fig schema. Fields not present in [`SuggestionObject`] (insertValue,
/// displayName, replaceValue, icon, isDangerous) are silently ignored by serde; v2
/// may add them.
///
/// `pub(crate)` to keep external callers from constructing entries that bypass
/// the `validate_arg_generators` invariant pass (empty names / hidden entries
/// are stripped there before any keystroke ever sees them).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum SuggestionEntry {
    Plain(String),
    Object(SuggestionObject),
}

impl SuggestionEntry {
    /// Returns true if this entry has no usable name.
    ///
    /// Covers both the empty-array case (`name: []`) and the blank-string case
    /// (`name: ""` or whitespace-only).  For an Object with any empty/whitespace
    /// name the whole entry is dropped — this is conservative but correct for
    /// the specs we know about.  If a future spec legitimately uses
    /// `["valid", ""]` with an intentional empty alias, loosen this check then.
    fn is_empty_name(&self) -> bool {
        match self {
            SuggestionEntry::Plain(s) => s.trim().is_empty(),
            SuggestionEntry::Object(o) => {
                o.name.is_empty() || o.name.iter().any(|n| n.trim().is_empty())
            }
        }
    }

    /// Returns true if the spec author explicitly marked this entry as hidden.
    /// Plain strings have no hidden field and therefore are never hidden.
    fn is_hidden(&self) -> bool {
        matches!(self, SuggestionEntry::Object(o) if o.hidden)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SuggestionObject {
    #[serde(default, deserialize_with = "deserialize_name_one_or_many")]
    pub(crate) name: Vec<String>,
    pub(crate) description: Option<String>,
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) priority: Option<Priority>,
    #[serde(default)]
    pub(crate) hidden: bool,
}

fn deserialize_name_one_or_many<'de, D>(d: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => Ok(vec![s]),
        OneOrMany::Many(v) => Ok(v),
    }
}

fn deserialize_suggestions_one_or_many<'de, D>(
    d: D,
) -> std::result::Result<Vec<SuggestionEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Fig allows the suggestions field to be either an array (canonical) or
    // a single entry. Mirror the existing `deserialize_args_one_or_many`
    // pattern so a malformed/single-entry spec still loads.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(SuggestionEntry),
        Many(Vec<SuggestionEntry>),
    }
    match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => Ok(vec![s]),
        OneOrMany::Many(v) => Ok(v),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub ttl_seconds: u64,
    #[serde(default)]
    pub cache_by_directory: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Release tag recording when a silently-mis-converted generator was corrected.
    /// Persists in the spec across regenerations so downstream consumers can
    /// enumerate and surface the affected specs on upgrade.
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
    /// Static enum-like suggestions from `args.suggestions` blocks at the
    /// resolved arg position. Populated by `collect_static_suggestions`.
    /// Surfaces via the engine candidate set unconditionally — these are
    /// values, not commands, so suppression flags do NOT apply.
    pub static_suggestions: Vec<Suggestion>,
    pub native_generators: Vec<String>,
    /// Native providers resolved from the spec (e.g.
    /// `arduino_cli_boards`). The engine dispatches these asynchronously
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
            priority: s.priority,
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
                priority: o.priority,
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
    let mut static_suggestions = Vec::new();

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
                collect_static_suggestions(&arg_spec.suggestions, &mut static_suggestions);
                match arg_spec.template.as_deref() {
                    Some("filepaths") => wants_filepaths = true,
                    Some("folders") => wants_folders_only = true,
                    _ => {}
                }
            }
        }
    }

    // Check positional arg specs at the resolved position, but only when
    // not filling a flag argument. When `preceding_flag_has_args` is true,
    // the user is supplying the flag's value — positional arg specs are
    // irrelevant and their suggestions would pollute the candidate set.
    if !preceding_flag_has_args {
        for arg_spec in current_args {
            collect_generators(
                &arg_spec.generators,
                &mut native_generators,
                &mut provider_generators,
                &mut script_generators,
                &mut wants_filepaths,
                &mut wants_folders_only,
            );
            collect_static_suggestions(&arg_spec.suggestions, &mut static_suggestions);
            match arg_spec.template.as_deref() {
                Some("filepaths") => wants_filepaths = true,
                Some("folders") => wants_folders_only = true,
                _ => {}
            }
        }
    }

    SpecResolution {
        subcommands: subcommand_suggestions,
        options: option_suggestions,
        static_suggestions,
        native_generators,
        provider_generators,
        script_generators,
        wants_filepaths,
        wants_folders_only,
        preceding_flag_has_args,
        past_double_dash: past_positional && ctx.args.iter().any(|a| a == "--"),
    }
}

/// Map Fig `Suggestion.type` strings to `SuggestionKind`.
/// Per `docs/COMPLETION_SPEC.md` ("type mapping" table under
/// `Static suggestions`): subcommand/option/file/folder map to their
/// equivalents; "arg", "special", "shortcut", "mixin", "auto-execute", and
/// missing/unknown all fall back to `EnumValue`. This runs on the keystroke
/// hot path via `resolve_spec`, so it MUST stay a pure mapping with no
/// logging — `validate_arg_generators` already warns once at load time about
/// unknown type strings (see `is_known_suggestion_type`).
fn suggestion_kind_from_type(s: Option<&str>) -> SuggestionKind {
    match s {
        Some("subcommand") => SuggestionKind::Subcommand,
        Some("option") => SuggestionKind::Flag,
        Some("file") => SuggestionKind::FilePath,
        Some("folder") => SuggestionKind::Directory,
        Some("arg") | Some("special") | Some("shortcut") | Some("mixin") | Some("auto-execute")
        | None => SuggestionKind::EnumValue,
        Some(_) => SuggestionKind::EnumValue, // load-time validation already warned
    }
}

/// Set of Fig `Suggestion.type` strings recognized by `suggestion_kind_from_type`.
/// Kept in sync with that function; used at load time by `validate_arg_generators`
/// to warn once per unknown type string instead of warning on every keystroke.
fn is_known_suggestion_type(s: &str) -> bool {
    matches!(
        s,
        "subcommand"
            | "option"
            | "file"
            | "folder"
            | "arg"
            | "special"
            | "shortcut"
            | "mixin"
            | "auto-execute"
    )
}

/// Lift static `SuggestionEntry` values into ranked-pool `Suggestion`s.
/// Plain strings become `EnumValue`; objects use their declared `type` →
/// `SuggestionKind` mapping via `suggestion_kind_from_type`.
/// Aliases in `name: ["a", "b"]` emit one `Suggestion` per alias (no dedup —
/// `nucleo` handles duplicates transparently).
fn collect_static_suggestions(entries: &[SuggestionEntry], out: &mut Vec<Suggestion>) {
    for entry in entries {
        // Defensive guard: `validate_arg_generators` already prunes empty-name
        // and hidden entries at load time, but `collect_static_suggestions`
        // is the last stop before the popup. Re-checking here means that a
        // future caller who skips validation (or a code path that resolves
        // an unvalidated `CompletionSpec`) cannot leak empty-text or hidden
        // entries into the ranked candidate set.
        if entry.is_empty_name() || entry.is_hidden() {
            continue;
        }
        match entry {
            SuggestionEntry::Plain(text) => {
                out.push(Suggestion {
                    text: text.clone(),
                    description: None,
                    kind: SuggestionKind::EnumValue,
                    source: SuggestionSource::Spec,
                    priority: None,
                    ..Default::default()
                });
            }
            SuggestionEntry::Object(obj) => {
                let kind = suggestion_kind_from_type(obj.kind.as_deref());
                for name in &obj.name {
                    out.push(Suggestion {
                        text: name.clone(),
                        description: obj.description.clone(),
                        kind,
                        source: SuggestionSource::Spec,
                        priority: obj.priority,
                        ..Default::default()
                    });
                }
            }
        }
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
        // result set merged together). Specs must not double-dispatch when
        // a generator names a native/provider type alongside a script.
        let handled_by_type = if let Some(ref gen_type) = gen.generator_type {
            if let Some(kind) = providers::kind_from_type_str(gen_type) {
                // Native provider — routed to the async provider
                // pipeline instead of the legacy native/script paths.
                // The provider IS the implementation; do not also push
                // onto `native` or fall through to the script branch
                // below.
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
                // the behavior that predates native provider dispatch.
                //
                // Only warn when there is no fallback script/script_template:
                // the message previously claimed "no completions will be
                // produced", which is false whenever a script IS present
                // (we fall through and run it below).
                if gen.script.is_none() && gen.script_template.is_none() {
                    tracing::warn!(
                        generator_type = %gen_type,
                        "unknown generator type and no script fallback — no completions will be produced"
                    );
                } else {
                    tracing::warn!(
                        generator_type = %gen_type,
                        "unknown generator type — falling through to script"
                    );
                }
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

    let original_suggestions_len = arg_spec.suggestions.len();
    arg_spec.suggestions.retain(|entry| {
        if entry.is_empty_name() {
            warnings.push(format!(
                "suggestion in {spec_name} has empty name; dropping"
            ));
            return false;
        }
        if entry.is_hidden() {
            // Silent drop — `hidden: true` is the spec author's explicit signal
            // to suppress this entry.  No warning needed.
            return false;
        }
        true
    });
    if arg_spec.suggestions.len() < original_suggestions_len {
        tracing::warn!(
            "{spec_name}: removed {} suggestion(s) (empty name or hidden)",
            original_suggestions_len - arg_spec.suggestions.len()
        );
    }

    // Surface unknown `type` strings once at load time. `suggestion_kind_from_type`
    // is on the keystroke hot path and must stay silent — emitting the warning
    // here means each unknown type shows up once per spec load instead of once
    // per keystroke. The entry itself is kept; `EnumValue` is a safe fallback.
    for entry in &arg_spec.suggestions {
        if let SuggestionEntry::Object(obj) = entry {
            if let Some(type_str) = &obj.kind {
                if !is_known_suggestion_type(type_str) {
                    warnings.push(format!(
                        "suggestion in {spec_name} has unknown `type` \"{type_str}\"; falling back to EnumValue"
                    ));
                }
            }
        }
    }
}

/// Approximate heap bytes owned by `spec`.
///
/// Sums `len()` for every heap-allocated `String` and `capacity()` for every
/// `Vec` in the spec tree. Length (not capacity) is the stable proxy for
/// content size — capacities vary by allocator and serde's internal
/// `String::reserve` calls, which would make the metric noisy across runs.
/// For regression detection, content size is the right signal.
///
/// The walk is iterative to avoid recursion-depth issues on deeply nested
/// specs. Accuracy is approximate; the goal is a stable number that detects
/// large regressions, not a byte-perfect heap profiler reading.
// `pub` (not `pub(crate)`): the criterion bench is a separate Cargo target
// in `benches/` and links to gc-suggest as an external consumer, so
// `pub(crate)` items would not be visible to it.
pub fn estimated_heap_bytes(spec: &CompletionSpec) -> usize {
    use crate::transform::{ParameterizedTransform, Transform};

    fn opt_string_heap(s: &Option<String>) -> usize {
        s.as_deref().map(str::len).unwrap_or(0)
    }
    fn transform_heap(t: &Transform) -> usize {
        match t {
            // Named transforms carry no heap-owned strings (they're Copy enums).
            Transform::Named(_) => 0,
            Transform::Parameterized(p) => match p {
                ParameterizedTransform::SplitOn { delimiter } => delimiter.len(),
                ParameterizedTransform::ErrorGuard {
                    starts_with,
                    contains,
                } => opt_string_heap(starts_with) + opt_string_heap(contains),
                ParameterizedTransform::Suffix { value } => value.len(),
                ParameterizedTransform::JsonExtractArray { split_on, .. } => {
                    opt_string_heap(split_on)
                }
                // Skip the compiled regex (not heap-walkable cleanly) and
                // JsonPath/usize-only variants (heap is negligible or
                // structurally fixed).
                ParameterizedTransform::Skip { .. }
                | ParameterizedTransform::Take { .. }
                | ParameterizedTransform::RegexExtract { .. }
                | ParameterizedTransform::JsonExtract { .. }
                | ParameterizedTransform::ColumnExtract { .. } => 0,
            },
        }
    }
    fn suggestion_entry_heap(entry: &SuggestionEntry) -> usize {
        match entry {
            SuggestionEntry::Plain(s) => s.len(),
            SuggestionEntry::Object(obj) => {
                let names: usize = obj.name.iter().map(|n| n.len()).sum();
                let names_vec = obj.name.capacity() * std::mem::size_of::<String>();
                let desc = opt_string_heap(&obj.description);
                let kind = opt_string_heap(&obj.kind);
                names + names_vec + desc + kind
            }
        }
    }
    fn generator_heap(g: &GeneratorSpec) -> usize {
        let gt = opt_string_heap(&g.generator_type);
        let script: usize = g
            .script
            .as_ref()
            .map(|v| {
                v.capacity() * std::mem::size_of::<String>()
                    + v.iter().map(|s| s.len()).sum::<usize>()
            })
            .unwrap_or(0);
        let script_tmpl: usize = g
            .script_template
            .as_ref()
            .map(|v| {
                v.capacity() * std::mem::size_of::<String>()
                    + v.iter().map(|s| s.len()).sum::<usize>()
            })
            .unwrap_or(0);
        // 180 specs carry inline JS source; this is the largest single field.
        let js = opt_string_heap(&g.js_source);
        let tmpl = opt_string_heap(&g.template);
        let transforms_vec = g.transforms.capacity() * std::mem::size_of::<Transform>();
        let transforms_inner: usize = g.transforms.iter().map(transform_heap).sum();
        gt + script + script_tmpl + js + tmpl + transforms_vec + transforms_inner
    }
    fn arg_spec_heap(arg: &ArgSpec) -> usize {
        let name = opt_string_heap(&arg.name);
        let desc = opt_string_heap(&arg.description);
        let gens_vec = arg.generators.capacity() * std::mem::size_of::<GeneratorSpec>();
        let gens: usize = arg.generators.iter().map(generator_heap).sum();
        let tmpl = opt_string_heap(&arg.template);
        let sugg_vec = arg.suggestions.capacity() * std::mem::size_of::<SuggestionEntry>();
        let sugg: usize = arg.suggestions.iter().map(suggestion_entry_heap).sum();
        name + desc + gens_vec + gens + tmpl + sugg_vec + sugg
    }
    fn option_spec_heap(opt: &OptionSpec) -> usize {
        let names: usize = opt.name.iter().map(|n| n.len()).sum();
        let names_vec = opt.name.capacity() * std::mem::size_of::<String>();
        let desc = opt_string_heap(&opt.description);
        let args = opt.args.as_ref().map(arg_spec_heap).unwrap_or(0);
        names + names_vec + desc + args
    }

    let mut total = spec.name.len()
        + opt_string_heap(&spec.description)
        + spec.args.capacity() * std::mem::size_of::<ArgSpec>()
        + spec.args.iter().map(arg_spec_heap).sum::<usize>()
        + spec.options.capacity() * std::mem::size_of::<OptionSpec>()
        + spec.options.iter().map(option_spec_heap).sum::<usize>()
        + spec.subcommands.capacity() * std::mem::size_of::<SubcommandSpec>();

    // Walk subcommands iteratively
    let mut stack: Vec<&SubcommandSpec> = spec.subcommands.iter().collect();
    while let Some(sub) = stack.pop() {
        total += sub.name.len();
        total += opt_string_heap(&sub.description);
        total += sub.args.capacity() * std::mem::size_of::<ArgSpec>();
        total += sub.args.iter().map(arg_spec_heap).sum::<usize>();
        total += sub.options.capacity() * std::mem::size_of::<OptionSpec>();
        total += sub.options.iter().map(option_spec_heap).sum::<usize>();
        total += sub.subcommands.capacity() * std::mem::size_of::<SubcommandSpec>();
        stack.extend(sub.subcommands.iter());
    }

    total
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
                suggestions: vec![],
            }),
            priority: None,
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
                        suggestions: vec![],
                    })
                } else {
                    None
                },
                priority: None,
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
                priority: None,
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
        // native-provider path.
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
        // alongside a script body would merge two result sets into the
        // same popup.
        //
        // This fixture targets the native `git_branches` arm because
        // `find_option`/`resolve_spec` test fixtures already exercise
        // the native git path; the provider arm shares the same
        // `handled_by_type` guard, so the invariant is covered by
        // `test_resolve_spec_routes_known_provider_to_provider_generators`
        // alongside this test.
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
        // block MUST still dispatch. This preserves the behavior that
        // predates native provider dispatch — specs that paired a junk
        // type string with a real script were relying on the script to
        // run.
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
        // still owns that string — falls through to `native_generators`.
        // Unknown types do not route to providers.
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

    #[test]
    fn test_resolve_spec_routes_known_provider_to_provider_generators() {
        // Every registered provider `"type"` string must land in
        // `res.provider_generators` and NOT in `native_generators` or
        // `script_generators`. If `kind_from_type_str` silently stops
        // mapping one of these (e.g. a typo in a future refactor), the
        // generator would fall through to the unknown-type warn path
        // and silently produce zero completions — no user-visible
        // error, just a broken provider. This test is the regression
        // guard for that class of drop.
        let provider_types: &[&str] = &[
            "ansible_doc_modules",
            "arduino_cli_boards",
            "arduino_cli_ports",
            "cargo_workspace_members",
            "defaults_domains",
            "makefile_targets",
            "mamba_envs",
            "multipass_list",
            "multipass_list_not_deleted",
            "multipass_list_deleted",
            "multipass_list_running",
            "multipass_list_stopped",
            "npm_scripts",
            "pandoc_input_formats",
            "pandoc_output_formats",
        ];
        for type_str in provider_types {
            let spec_json = format!(
                r#"{{
                    "name": "test-provider-{type_str}",
                    "args": [{{
                        "name": "target",
                        "generators": [{{"type": "{type_str}"}}]
                    }}]
                }}"#
            );
            let spec: CompletionSpec = serde_json::from_str(&spec_json).unwrap();
            let ctx = CommandContext {
                command: Some(format!("test-provider-{type_str}")),
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
            assert_eq!(
                res.provider_generators.len(),
                1,
                "provider type {type_str:?} must route to provider_generators"
            );
            assert!(
                res.native_generators.is_empty(),
                "provider type {type_str:?} must NOT also appear in native_generators: {:?}",
                res.native_generators
            );
            assert!(
                res.script_generators.is_empty(),
                "provider type {type_str:?} must NOT also dispatch a script: {:?}",
                res.script_generators
            );
            let expected_kind = providers::kind_from_type_str(type_str)
                .unwrap_or_else(|| panic!("kind_from_type_str({type_str:?}) returned None"));
            assert_eq!(
                res.provider_generators[0], expected_kind,
                "wrong ProviderKind variant for {type_str:?}"
            );
        }
    }

    #[test]
    fn test_generator_spec_rejects_unknown_fields() {
        // Silent-drop class of bug: a spec that uses a singular "transform"
        // key (rather than the correct "transforms") previously parsed
        // cleanly and silently dropped the transform pipeline, because
        // `GeneratorSpec` had no `deny_unknown_fields`. `#[serde(deny_unknown_fields)]`
        // on the struct turns that into a hard parse error — this test
        // pins the invariant so a future refactor cannot quietly remove
        // the attribute.
        let bad = r#"{"script": ["echo"], "transform": ["split_lines"]}"#;
        let err = serde_json::from_str::<GeneratorSpec>(bad).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("transform") || msg.contains("unknown field"),
            "error should identify the offending unknown field: {msg}"
        );
    }

    #[test]
    fn parses_priority_from_subcommand_spec() {
        let json = r#"{
            "name": "checkout",
            "description": "switch branches",
            "priority": 90
        }"#;
        let parsed: SubcommandSpec = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.priority, Some(Priority::new(90)));
    }

    #[test]
    fn missing_priority_field_is_none() {
        let json = r#"{
            "name": "checkout",
            "description": "switch branches"
        }"#;
        let parsed: SubcommandSpec = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.priority, None);
    }

    #[test]
    fn subcommand_priority_propagates_to_suggestion() {
        let json = r#"{
            "name": "git",
            "subcommands": [
                { "name": "checkout", "priority": 95 }
            ]
        }"#;
        let spec: CompletionSpec = serde_json::from_str(json).unwrap();
        let ctx = CommandContext {
            command: Some("git".into()),
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
        let resolution = resolve_spec(&spec, &ctx);
        let checkout = resolution
            .subcommands
            .iter()
            .find(|s| s.text == "checkout")
            .expect("checkout subcommand should be present");
        assert_eq!(checkout.priority, Some(Priority::new(95)));
    }

    #[test]
    fn nested_subcommand_priority_propagates_to_suggestion() {
        // `git remote add` lives two levels deep in the spec. The audit
        // tool's recursion is supposed to bump nested subcommands too;
        // verify the override actually surfaces through `resolve_spec`
        // when the cursor lands at the nested completion site.
        let json = r#"{
            "name": "git",
            "subcommands": [
                {
                    "name": "remote",
                    "priority": 72,
                    "subcommands": [
                        { "name": "add", "priority": 85 },
                        { "name": "rm" }
                    ]
                }
            ]
        }"#;
        let spec: CompletionSpec = serde_json::from_str(json).unwrap();
        let ctx = CommandContext {
            command: Some("git".into()),
            args: vec!["remote".into()],
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
        let resolution = resolve_spec(&spec, &ctx);
        let add = resolution
            .subcommands
            .iter()
            .find(|s| s.text == "add")
            .expect("nested `add` subcommand should be present");
        assert_eq!(add.priority, Some(Priority::new(85)));
        let rm = resolution
            .subcommands
            .iter()
            .find(|s| s.text == "rm")
            .expect("nested `rm` subcommand should be present");
        // Sibling without an explicit priority must still report None so
        // the ranker can fall back to the kind base.
        assert_eq!(rm.priority, None);
    }

    #[test]
    fn option_priority_propagates_to_every_alias() {
        // Multi-alias options collapse into one OptionSpec but one Suggestion
        // per alias. `priority` should ride along on every alias so the
        // ranker scores `-r` and `--recursive` identically.
        let json = r#"{
            "name": "rsync",
            "options": [
                { "name": ["-r", "--recursive"], "priority": 70 }
            ]
        }"#;
        let spec: CompletionSpec = serde_json::from_str(json).unwrap();
        let ctx = CommandContext {
            command: Some("rsync".into()),
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
        let resolution = resolve_spec(&spec, &ctx);
        let r = resolution
            .options
            .iter()
            .find(|s| s.text == "-r")
            .expect("`-r` flag suggestion should be present");
        let recursive = resolution
            .options
            .iter()
            .find(|s| s.text == "--recursive")
            .expect("`--recursive` flag suggestion should be present");
        assert_eq!(r.priority, Some(Priority::new(70)));
        assert_eq!(recursive.priority, Some(Priority::new(70)));
        assert_eq!(r.kind, SuggestionKind::Flag);
        assert_eq!(recursive.kind, SuggestionKind::Flag);
    }

    #[test]
    fn test_generator_spec_accepts_all_declared_fields() {
        // Companion to the deny_unknown_fields test above: ensure every
        // field currently on `GeneratorSpec` still deserializes cleanly
        // when set together. If someone removes a field without updating
        // the corpus, this catches it before the full spec corpus would.
        let ok = r#"{
            "type": "git_branches",
            "script": ["echo"],
            "script_template": ["echo", "{current_token}"],
            "transforms": ["split_lines"],
            "cache": {"ttl_seconds": 60, "cache_by_directory": true},
            "requires_js": false,
            "js_source": "module.exports = {}",
            "_corrected_in": "v0.10.0",
            "template": "filepaths"
        }"#;
        let gen: GeneratorSpec = serde_json::from_str(ok).unwrap();
        assert_eq!(gen.generator_type.as_deref(), Some("git_branches"));
        assert_eq!(gen.transforms.len(), 1);
        assert_eq!(gen.corrected_in.as_deref(), Some("v0.10.0"));
        assert_eq!(gen.template.as_deref(), Some("filepaths"));
    }

    #[test]
    fn static_suggestions_deserialize_plain_and_object() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
            "name": "x",
            "args": {
                "name": "y",
                "suggestions": ["plain", {"name": "obj", "description": "d"}]
            }
        }"#,
        )
        .unwrap();
        let arg = &spec.args[0];
        assert_eq!(arg.suggestions.len(), 2);
        match &arg.suggestions[0] {
            SuggestionEntry::Plain(s) => assert_eq!(s, "plain"),
            _ => panic!("expected Plain"),
        }
        match &arg.suggestions[1] {
            SuggestionEntry::Object(o) => {
                assert_eq!(o.name, vec!["obj".to_string()]);
                assert_eq!(o.description.as_deref(), Some("d"));
            }
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn sanitize_strips_control_chars_in_suggestion_name() {
        // The JSON uses \u001b (the valid JSON unicode escape for ESC = 0x1B).
        // serde_json parses \u001b into an actual ESC byte inside the Rust
        // String; sanitize_string then strips it because ESC is a control char.
        //   "ev\u001bil"  -> parsed as "ev\x1bil" -> sanitized to "evil"
        //   "d\u001b"     -> parsed as "d\x1b"    -> sanitized to "d"
        //   "pl\u001bain" -> parsed as "pl\x1bain"-> sanitized to "plain"
        let json = "{\"name\":\"x\",\"args\":{\"name\":\"y\",\"suggestions\":[{\"name\":\"ev\\u001bil\",\"description\":\"d\\u001b\"},\"pl\\u001bain\"]}}";
        let spec = parse_spec_checked_and_sanitized(json).unwrap();
        let arg = &spec.args[0];
        match &arg.suggestions[0] {
            SuggestionEntry::Object(o) => {
                assert_eq!(o.name[0], "evil");
                assert_eq!(o.description.as_deref(), Some("d"));
            }
            _ => panic!("expected Object"),
        }
        match &arg.suggestions[1] {
            SuggestionEntry::Plain(s) => assert_eq!(s, "plain"),
            _ => panic!("expected Plain"),
        }
    }

    #[test]
    fn empty_suggestion_names_are_pruned_with_warning() {
        let json = r#"{
            "name": "x",
            "args": {
                "name": "y",
                "suggestions": [
                    {"name": []},
                    {"name": ""},
                    "ok"
                ]
            }
        }"#;
        let mut spec = parse_spec_checked_and_sanitized(json).unwrap();
        let warnings = validate_spec_generators(&mut spec);
        assert_eq!(
            spec.args[0].suggestions.len(),
            1,
            "only 'ok' should survive pruning"
        );
        match &spec.args[0].suggestions[0] {
            SuggestionEntry::Plain(s) => assert_eq!(s, "ok"),
            _ => panic!("expected Plain(\"ok\")"),
        }
        assert_eq!(
            warnings.len(),
            2,
            "expected two warnings (one per empty entry)"
        );
        for w in &warnings {
            assert!(
                w.contains('x'),
                "warning should contain the spec name 'x', got: {w}"
            );
        }
    }

    #[test]
    fn hidden_suggestion_is_dropped_at_load_time() {
        let json = r#"{
            "name": "x",
            "args": {
                "name": "y",
                "suggestions": [
                    {"name": "visible"},
                    {"name": "hush", "hidden": true},
                    "plain-also-visible"
                ]
            }
        }"#;
        let mut spec = parse_spec_checked_and_sanitized(json).unwrap();
        let warnings = validate_spec_generators(&mut spec);
        assert!(
            warnings.is_empty(),
            "hidden entries should be dropped silently"
        );
        let names: Vec<&str> = spec.args[0]
            .suggestions
            .iter()
            .map(|e| match e {
                SuggestionEntry::Plain(s) => s.as_str(),
                SuggestionEntry::Object(o) => o.name[0].as_str(),
            })
            .collect();
        assert_eq!(names, vec!["visible", "plain-also-visible"]);
    }

    #[test]
    fn test_resolve_static_suggestions_positional() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{"name":"foo","args":[{"name":"fmt","suggestions":["a","b"]}]}"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("foo".into()),
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
        assert_eq!(res.static_suggestions.len(), 2);
        let texts: Vec<&str> = res
            .static_suggestions
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(texts.contains(&"a"));
        assert!(texts.contains(&"b"));
        assert!(res
            .static_suggestions
            .iter()
            .all(|s| s.kind == crate::types::SuggestionKind::EnumValue));
        assert!(res
            .static_suggestions
            .iter()
            .all(|s| s.source == crate::types::SuggestionSource::Spec));
    }

    #[test]
    fn test_static_suggestion_type_field_maps_to_kind() {
        use crate::types::SuggestionKind;

        let spec: CompletionSpec = serde_json::from_str(
            r#"{
            "name":"foo",
            "args":[{"name":"x","suggestions":[
                {"name":"sub","type":"subcommand"},
                {"name":"opt","type":"option"},
                {"name":"file","type":"file"},
                {"name":"folder","type":"folder"},
                {"name":"defaulted"},
                {"name":"argish","type":"arg"},
                {"name":"specialish","type":"special"},
                {"name":"sh","type":"shortcut"},
                {"name":"mx","type":"mixin"},
                {"name":"ae","type":"auto-execute"},
                {"name":"unknown","type":"made_up_xyz"}
            ]}]
        }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("foo".into()),
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
        let by_text: std::collections::HashMap<String, SuggestionKind> = res
            .static_suggestions
            .into_iter()
            .map(|s| (s.text, s.kind))
            .collect();
        assert_eq!(by_text["sub"], SuggestionKind::Subcommand);
        assert_eq!(by_text["opt"], SuggestionKind::Flag);
        assert_eq!(by_text["file"], SuggestionKind::FilePath);
        assert_eq!(by_text["folder"], SuggestionKind::Directory);
        assert_eq!(by_text["defaulted"], SuggestionKind::EnumValue);
        assert_eq!(by_text["argish"], SuggestionKind::EnumValue);
        assert_eq!(by_text["specialish"], SuggestionKind::EnumValue);
        assert_eq!(by_text["sh"], SuggestionKind::EnumValue);
        assert_eq!(by_text["mx"], SuggestionKind::EnumValue);
        assert_eq!(by_text["ae"], SuggestionKind::EnumValue);
        assert_eq!(by_text["unknown"], SuggestionKind::EnumValue);
    }

    #[test]
    fn unknown_suggestion_type_warns_at_load_time() {
        let json = r#"{"name":"x","args":{"name":"y","suggestions":[
            {"name":"a","type":"made_up_xyz"},
            {"name":"b","type":"file"}
        ]}}"#;
        let mut spec = parse_spec_checked_and_sanitized(json).unwrap();
        let warnings = validate_spec_generators(&mut spec);
        assert!(warnings.iter().any(|w| w.contains("made_up_xyz")));
        assert!(!warnings.iter().any(|w| w.contains("\"file\"")));
        assert_eq!(
            spec.args[0].suggestions.len(),
            2,
            "unknown-type entry should NOT be dropped"
        );
    }

    #[test]
    fn suggestion_object_ignores_reserved_fig_fields() {
        // Reserved Fig fields not modeled on `SuggestionObject` must remain
        // silently ignored by serde. A future `#[serde(deny_unknown_fields)]`
        // would otherwise break parsing of real bundled specs that carry
        // `insertValue`, `displayName`, `replaceValue`, `icon`,
        // `isDangerous`, or `deprecated`.
        let json = r#"{
            "name": "x",
            "args": {
                "name": "y",
                "suggestions": [{
                    "name": "a",
                    "description": "desc",
                    "insertValue": "a ",
                    "displayName": "Alpha",
                    "replaceValue": "alpha",
                    "icon": "fig://icon?type=string",
                    "isDangerous": true,
                    "deprecated": true
                }]
            }
        }"#;
        let mut spec = parse_spec_checked_and_sanitized(json).unwrap();
        let warnings = validate_spec_generators(&mut spec);
        assert_eq!(
            spec.args[0].suggestions.len(),
            1,
            "entry with reserved fields should parse and survive validation"
        );
        match &spec.args[0].suggestions[0] {
            SuggestionEntry::Object(o) => {
                assert_eq!(o.name, vec!["a".to_string()]);
                assert_eq!(o.description.as_deref(), Some("desc"));
            }
            _ => panic!("expected Object"),
        }
        assert!(
            warnings.is_empty(),
            "reserved Fig fields must not produce warnings, got: {warnings:?}"
        );
    }

    #[test]
    fn embedded_specs_under_memory_budget() {
        // Measured baseline: ~37.5 MB (37,536,540 bytes), measured 2026-04-28
        // on 709 specs. The `estimated_heap_bytes` walk covers the whole
        // `CompletionSpec` tree (js_source, transforms, descriptions, etc.).
        // 64 MiB (67,108,864 bytes) gives ~1.78x headroom for spec corpus
        // growth before requiring a deliberate budget raise.
        const BUDGET_BYTES: usize = 64 * 1024 * 1024;
        let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
        let store = SpecStore::load_from_dir(&spec_dir).unwrap().store;
        let total: usize = store.iter().map(|(_, s)| estimated_heap_bytes(s)).sum();
        assert!(
            total < BUDGET_BYTES,
            "embedded specs heap {} bytes exceeds budget {} bytes — investigate before raising the limit",
            total,
            BUDGET_BYTES
        );
        eprintln!(
            "INFO: embedded specs estimated heap: {} bytes ({} KB)",
            total,
            total / 1024
        );
    }

    #[test]
    fn preceding_flag_args_suppress_positional_static_and_generators() {
        // Invariant: filling a flag's argument must not also collect
        // positional-arg generators or static suggestions. Mixing them
        // produces wrong candidates (e.g. for templated flags like
        // `-r filepaths`, where positional package-name generators would
        // otherwise leak in alongside the file completions).
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "pip",
                "subcommands": [{
                    "name": "install",
                    "options": [{
                        "name": ["-r"],
                        "args": { "name": "file", "template": "filepaths" }
                    }],
                    "args": [{
                        "name": "pkg",
                        "suggestions": ["pos1", "pos2"],
                        "generators": [{"type": "git_branches"}]
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
        assert!(res.preceding_flag_has_args);
        assert!(
            res.static_suggestions.is_empty(),
            "positional static suggestions must NOT leak when filling a flag arg: {:?}",
            res.static_suggestions
        );
        assert!(
            res.native_generators.is_empty(),
            "positional native generators must NOT leak when filling a flag arg: {:?}",
            res.native_generators
        );
        assert!(res.wants_filepaths);
    }

    #[test]
    fn static_suggestion_priority_field_round_trips() {
        // `collect_static_suggestions` copies `obj.priority` into the
        // resulting Suggestion. Pin the round-trip so a regression that
        // drops or replaces the priority field is caught.
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "foo",
                "args": [{
                    "name": "x",
                    "suggestions": [
                        {"name": "x", "priority": 90},
                        {"name": "y"}
                    ]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("foo".into()),
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
        let by_text: HashMap<String, Option<Priority>> = res
            .static_suggestions
            .iter()
            .map(|s| (s.text.clone(), s.priority))
            .collect();
        assert_eq!(by_text["x"], Some(Priority::new(90)));
        assert_eq!(by_text["y"], None);
    }

    #[test]
    fn static_suggestions_accept_singular_string_and_object() {
        // Fig schema permits `suggestions` as a singular form (string or
        // object) as well as an array. Likewise `name` inside a
        // SuggestionObject. Exercise the One arm of every OneOrMany so a
        // regression that only kept the Many path is caught.
        let plain: CompletionSpec =
            serde_json::from_str(r#"{"name":"a","args":{"name":"x","suggestions":"foo"}}"#)
                .unwrap();
        assert_eq!(plain.args[0].suggestions.len(), 1);
        match &plain.args[0].suggestions[0] {
            SuggestionEntry::Plain(s) => assert_eq!(s, "foo"),
            _ => panic!("expected Plain singular"),
        }

        let obj: CompletionSpec = serde_json::from_str(
            r#"{"name":"a","args":{"name":"x","suggestions":{"name":"bar"}}}"#,
        )
        .unwrap();
        assert_eq!(obj.args[0].suggestions.len(), 1);
        match &obj.args[0].suggestions[0] {
            SuggestionEntry::Object(o) => assert_eq!(o.name, vec!["bar".to_string()]),
            _ => panic!("expected Object singular"),
        }

        let str_name: CompletionSpec = serde_json::from_str(
            r#"{"name":"a","args":{"name":"x","suggestions":[{"name":"singlestr"}]}}"#,
        )
        .unwrap();
        match &str_name.args[0].suggestions[0] {
            SuggestionEntry::Object(o) => {
                assert_eq!(o.name, vec!["singlestr".to_string()]);
            }
            _ => panic!("expected Object with singular name"),
        }
    }

    #[test]
    fn option_arg_static_suggestions_emit_one_per_alias() {
        // `collect_static_suggestions` is invoked from both the positional
        // and the preceding_flag paths. Cover the latter with a multi-alias
        // name array — a regression that emitted only the first alias on
        // the option-arg path (vs the positional path) wouldn't be caught
        // by the existing tests.
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "fmt",
                "options": [{
                    "name": ["--format"],
                    "args": {
                        "name": "kind",
                        "suggestions": [
                            {"name": ["json", "j"], "description": "JSON output"}
                        ]
                    }
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("fmt".into()),
            args: vec!["--format".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("--format".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        assert_eq!(res.static_suggestions.len(), 2);
        for s in &res.static_suggestions {
            assert_eq!(s.description.as_deref(), Some("JSON output"));
            assert_eq!(s.kind, SuggestionKind::EnumValue);
        }
        let texts: Vec<&str> = res
            .static_suggestions
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(texts.contains(&"json"));
        assert!(texts.contains(&"j"));
    }

    #[test]
    fn pure_control_char_suggestion_name_pruned_after_sanitize() {
        // The combined sanitize → validate pipeline must drop entries whose
        // names sanitize down to empty strings. A regression that runs
        // validation before sanitize, or skips the post-sanitize empty
        // check, would leak an empty-text suggestion to the popup.
        let json = "{\"name\":\"x\",\"args\":{\"name\":\"y\",\"suggestions\":[{\"name\":\"\\u0001\\u0002\"},\"ok\"]}}";
        let mut spec = parse_spec_checked_and_sanitized(json).unwrap();
        let _ = validate_spec_generators(&mut spec);
        assert_eq!(spec.args[0].suggestions.len(), 1);
        match &spec.args[0].suggestions[0] {
            SuggestionEntry::Plain(s) => assert_eq!(s, "ok"),
            _ => panic!("expected Plain(\"ok\") to be the sole survivor"),
        }
    }

    #[test]
    fn duplicate_suggestion_names_emit_both_entries() {
        // `collect_static_suggestions` documents "no dedup — nucleo handles
        // duplicates transparently". Pin that contract so a future change
        // that introduces dedup at the spec layer is caught.
        let spec: CompletionSpec = serde_json::from_str(
            r#"{"name":"d","args":[{"name":"x","suggestions":["foo","foo"]}]}"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("d".into()),
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
        assert_eq!(res.static_suggestions.len(), 2);
        assert!(res.static_suggestions.iter().all(|s| s.text == "foo"));
    }
}
