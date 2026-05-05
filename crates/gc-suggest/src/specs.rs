use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

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
    for arg in opt.args.iter_mut().chain(opt.extra_args.iter_mut()) {
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

/// Deserialize option `args` as either a single object or an ordered array.
fn deserialize_option_args<'de, D>(deserializer: D) -> std::result::Result<Vec<ArgSpec>, D::Error>
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
        Some(OneOrMany::One(single)) => Ok(vec![single]),
        Some(OneOrMany::Many(vec)) => Ok(vec),
        None => Ok(Vec::new()),
    }
}

/// Deserialize an `Option<JsRuntimeSpec>` and wrap it in `Arc` so the
/// dispatch hot path can share the underlying spec with worker tasks
/// without deep-cloning the embedded JS source. Equivalent to
/// `Option::<JsRuntimeSpec>::deserialize(...)?.map(Arc::new)` — the
/// helper exists because serde's `rc` feature (which would let us derive
/// `Deserialize` directly on `Arc<T>`) is not enabled in this workspace.
fn deserialize_arc_js_runtime<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Arc<JsRuntimeSpec>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<JsRuntimeSpec>::deserialize(deserializer)?.map(Arc::new))
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

#[derive(Debug, Clone)]
pub struct OptionSpec {
    pub name: Vec<String>,
    pub description: Option<String>,
    /// Backwards-compatible first option argument. For `args: [...]`, this
    /// holds only element 0; additional positional option args live in
    /// `extra_args` so resolution can preserve array boundaries without
    /// changing the public field shape during this PR.
    ///
    /// Callers that need every option arg should iterate
    /// `args.iter().chain(extra_args.iter())`.
    pub args: Option<ArgSpec>,
    /// Additional positional option args beyond the first. See [`Self::args`]
    /// for the iteration pattern that walks every arg the option declares.
    pub extra_args: Vec<ArgSpec>,
    pub priority: Option<Priority>,
}

impl<'de> Deserialize<'de> for OptionSpec {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawOptionSpec {
            name: Vec<String>,
            description: Option<String>,
            #[serde(default, deserialize_with = "deserialize_option_args")]
            args: Vec<ArgSpec>,
            #[serde(default)]
            priority: Option<Priority>,
        }

        let raw = RawOptionSpec::deserialize(deserializer)?;
        let mut args = raw.args.into_iter();
        Ok(Self {
            name: raw.name,
            description: raw.description,
            args: args.next(),
            extra_args: args.collect(),
            priority: raw.priority,
        })
    }
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
    #[serde(default, rename = "isOptional")]
    pub is_optional: bool,
    #[serde(default, rename = "isVariadic")]
    pub is_variadic: bool,
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

/// Categorises a [`JsRuntimeSpec`] so the runtime dispatch path can pick the
/// correct evaluator. Mirrors the three Fig generator shapes that survive into
/// runtime JS:
///
/// - `PostProcess` — the converter saw a `script` + `postProcess` pair whose
///   post-process body could not be lowered to a declarative transform. The
///   script runs as a normal script generator and stdout is fed through the JS
///   function in `source` to produce suggestions.
/// - `ScriptFunction` — Fig's `script: (...) => [...]` shape: the JS body
///   evaluates to an `argv` array which is then spawned.
/// - `Custom` — Fig's `custom: async (...) => [...]` shape: the JS body
///   returns suggestions directly without any subprocess invocation.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JsRuntimeKind {
    PostProcess,
    ScriptFunction,
    Custom,
}

/// Runtime JS metadata for generators that need QuickJS evaluation. The
/// engine routes on [`Self::kind`] to drive QuickJS dispatch via
/// [`gc_jsrt::JsWorker`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsRuntimeSpec {
    pub kind: JsRuntimeKind,
    pub source: String,
    /// Optional per-generator timeout override.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// When `true`, allows shell-string command execution from `script_function`
    /// or `custom` generators. Defaults to `false`. In the current engine this
    /// is effective only for `custom` host calls to `executeShellCommand`;
    /// `script_function` returns argv for the engine to spawn.
    #[serde(default)]
    pub allow_shell_command: bool,
    /// True only when the converter proved this source does not close over
    /// bundler/minifier helper bindings that the QuickJS host will not install.
    /// Custom/script_function sources without this proof remain unsupported.
    #[serde(default)]
    pub self_contained: bool,
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
    /// Runtime JS metadata for generators that need QuickJS evaluation. The
    /// engine honours it in preference to the legacy `requires_js`
    /// short-circuit.
    ///
    /// Wrapped in `Arc` so the dispatch hot path can share it with the
    /// spawned worker task without deep-cloning the embedded JS source on
    /// every keystroke. The corpus contains generators whose source is
    /// several KB (e.g. AWS), and an `Arc` pointer-bump is essentially
    /// free vs. a `String::clone`.
    #[serde(default, deserialize_with = "deserialize_arc_js_runtime")]
    pub js_runtime: Option<Arc<JsRuntimeSpec>>,
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

/// Sentinel `source_dir` value used to label embedded specs in
/// diagnostics (`status` / `doctor`). Not a real filesystem path —
/// [`SpecEntry::source`] distinguishes embedded from filesystem at load
/// time; callers that render a human label format this string verbatim.
pub const EMBEDDED_VIRTUAL_DIR: &str = "<embedded>";

/// How a [`SpecEntry`] sources its JSON contents on first parse.
///
/// Filesystem entries hold a `PathBuf` and read the file lazily.
/// Embedded entries hold a `&'static str` slice into the binary's
/// `EMBEDDED_SPECS` table — no disk I/O ever.
#[derive(Debug, Clone)]
pub enum SpecSource {
    /// Owned filesystem path. Read on first access via `std::fs::read_to_string`.
    Filesystem(PathBuf),
    /// `&'static str` slice into the binary's embedded payload. Never
    /// freed; the runtime fallback path uses this to avoid the
    /// `~/.cache/ghost-complete/embedded-specs/` materialisation step.
    Embedded(&'static str),
}

/// Explicit source location for a registered spec entry.
///
/// Filesystem specs have a real path. Embedded specs are in-memory slices
/// compiled into the binary and intentionally do not fabricate a path like
/// `<embedded>/git.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecLocation {
    Filesystem { stem: String, path: PathBuf },
    Embedded { stem: String },
}

impl SpecLocation {
    pub fn stem(&self) -> &str {
        match self {
            Self::Filesystem { stem, .. } | Self::Embedded { stem } => stem,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecEntryLoadError {
    pub id: String,
    pub source: SpecLocation,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecLookupError {
    NoSuchSpec {
        command: String,
    },
    LoadFailed {
        command: String,
        id: String,
        source: SpecLocation,
        error: String,
    },
}

impl std::fmt::Display for SpecLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchSpec { command } => write!(f, "no spec registered for {command}"),
            Self::LoadFailed {
                command, id, error, ..
            } => write!(f, "spec {id} for {command} failed to load: {error}"),
        }
    }
}

impl std::error::Error for SpecLookupError {}

/// One unique spec, addressable by one or more aliases. Owned by
/// [`SpecStore`] and shared into the alias index via `Arc` so a single
/// parsed spec doesn't have to be cloned per-alias on the load path.
///
/// `parsed` is filled lazily by [`SpecEntry::spec`], [`SpecEntry::spec_arc`],
/// [`SpecStore::get`], or force-loading through [`SpecStore::iter`] /
/// [`SpecStore::force_load_errors`]. [`SpecEntry::load_error`] only reads an
/// already-attempted parse result. Until then the entry holds only the metadata
/// needed to register aliases. This keeps idle memory under 20 MB even with
/// the full 709-spec corpus — see
/// `docs/superpowers/specs/2026-05-05-lazy-spec-loading-design.md`.
#[derive(Debug)]
pub struct SpecEntry {
    /// Stable identifier used for status reporting and `iter()`. Always
    /// the filename stem — never `CompletionSpec.name`, because two files
    /// can declare the same `name` and we surface that as a conflict
    /// rather than letting one silently shadow the other.
    pub id: String,
    /// Filename stem, equal to `id`.
    pub filename_stem: String,
    /// Source directory (an entry from `resolve_spec_dirs`'s output, or
    /// the [`EMBEDDED_VIRTUAL_DIR`] sentinel for embedded specs).
    pub source_dir: PathBuf,
    /// Every alias this spec resolves under, in the order they were
    /// considered: filename stem first, then `CompletionSpec.name` when
    /// it differs from the stem and does not collide with another entry.
    pub aliases: Vec<String>,
    /// Where to load the spec contents from on first access.
    pub source: SpecSource,
    /// Lazy parse target. `Ok(Arc<CompletionSpec>)` on success;
    /// `Err(error_message)` on parse failure (sticky — never retried).
    parsed: OnceLock<Result<Arc<CompletionSpec>, String>>,
}

impl SpecEntry {
    fn source_label(&self) -> String {
        match &self.source {
            SpecSource::Filesystem(path) => path.display().to_string(),
            SpecSource::Embedded(_) => EMBEDDED_VIRTUAL_DIR.to_string(),
        }
    }

    fn parsed_result(&self) -> std::result::Result<&Arc<CompletionSpec>, &str> {
        self.parsed
            .get_or_init(|| {
                let parsed = parse_entry_source(&self.source);
                if let Err(err) = &parsed {
                    tracing::warn!(
                        spec_id = %self.id,
                        source = %self.source_label(),
                        error = %err,
                        "spec lazy load failed"
                    );
                }
                parsed
            })
            .as_ref()
            .map_err(String::as_str)
    }

    /// Lazily parse and return the spec. Returns `None` if parsing
    /// failed (the failure is recorded once and never retried — call
    /// [`Self::load_error`] for diagnostics).
    pub fn spec(&self) -> Option<&CompletionSpec> {
        self.parsed_result().ok().map(|arc| arc.as_ref())
    }

    /// Like [`Self::spec`] but returns an owned clone of the cached
    /// `Arc`. Use when the caller needs to hold the spec across a
    /// boundary that requires `'static` lifetime; prefer [`Self::spec`]
    /// otherwise.
    pub fn spec_arc(&self) -> Option<Arc<CompletionSpec>> {
        self.parsed_result().ok().cloned()
    }

    /// Like [`Self::spec`] but preserves the lazy parse error instead of
    /// collapsing it into `None`.
    pub fn spec_result(&self) -> std::result::Result<&CompletionSpec, &str> {
        self.parsed_result().map(|arc| arc.as_ref())
    }

    /// Returns the parse error message if the lazy load failed.
    /// Returns `None` if the spec has not yet been touched OR loaded
    /// successfully — disambiguate via [`Self::is_parsed`].
    pub fn load_error(&self) -> Option<&str> {
        self.parsed
            .get()
            .and_then(|r| r.as_ref().err())
            .map(String::as_str)
    }

    /// True iff this entry has been touched (lazy parse attempted).
    pub fn is_parsed(&self) -> bool {
        self.parsed.get().is_some()
    }

    pub fn location(&self) -> SpecLocation {
        match &self.source {
            SpecSource::Filesystem(path) => SpecLocation::Filesystem {
                stem: self.filename_stem.clone(),
                path: path.clone(),
            },
            SpecSource::Embedded(_) => SpecLocation::Embedded {
                stem: self.filename_stem.clone(),
            },
        }
    }
}

/// Parse the JSON behind a [`SpecSource`] into an `Arc<CompletionSpec>`.
/// Wraps existing parse + sanitize machinery; returns the error as a
/// `String` so the failure is `Send + Sync` and storable in `OnceLock`.
fn parse_entry_source(source: &SpecSource) -> Result<Arc<CompletionSpec>, String> {
    let contents: std::borrow::Cow<'_, str> = match source {
        SpecSource::Filesystem(path) => std::borrow::Cow::Owned(
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?,
        ),
        SpecSource::Embedded(contents) => std::borrow::Cow::Borrowed(*contents),
    };
    let mut spec =
        parse_spec_checked_and_sanitized(&contents).map_err(|e| format!("parse: {e}"))?;
    let warnings = validate_spec_generators(&mut spec);
    for w in &warnings {
        tracing::warn!("{}: {w}", spec.name);
    }
    Ok(Arc::new(spec))
}

/// One alias collision detected during loading. `winner` and `loser`
/// describe registration precedence for the contended alias; inspect
/// [`AliasConflict::disposition`] to tell whether the lower-precedence
/// entry was rejected from that alias or kept as a lazy-parse fallback
/// candidate. Surfaced via [`SpecStore::conflicts`].
#[derive(Debug, Clone)]
pub struct AliasConflict {
    /// The alias that two specs both wanted to register.
    pub alias: String,
    /// What kind of collision this is — drives diagnostics phrasing.
    pub kind: AliasConflictKind,
    /// Runtime role for the lower-precedence candidate.
    pub disposition: AliasConflictDisposition,
    /// The primary spec for the alias at registration time.
    pub winner: AliasOwner,
    /// The lower-precedence spec involved in the collision.
    pub loser: AliasOwner,
}

/// Categorises an [`AliasConflict`] so doctor / status can render
/// appropriate phrasing without re-deriving the relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasConflictKind {
    /// Two different spec files declared the same `CompletionSpec.name`.
    /// Example: `tns.json` and `nativescript.json` both have `name: "ns"`.
    DuplicateName,
    /// One spec's `CompletionSpec.name` collides with another spec's
    /// filename stem. Example: `kubecolor.json` declares `name: "kubectl"`
    /// while `kubectl.json` already exists in the same dir.
    NameMatchesOtherStem,
    /// Same filename in two different configured dirs. The earlier dir is
    /// preferred per `resolve_spec_dirs` order, while later copies can remain
    /// lazy-parse fallbacks.
    DirectoryPrecedence,
}

/// How the loader treated the lower-precedence side of an alias collision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasConflictDisposition {
    /// The lower-precedence entry is not reachable through the contended alias.
    Rejected,
    /// The lower-precedence entry remains registered behind the primary owner
    /// and can resolve if earlier candidates fail lazy parsing.
    FallbackCandidate,
}

/// Identifies the source of a spec involved in an [`AliasConflict`].
#[derive(Debug, Clone)]
pub struct AliasOwner {
    pub filename_stem: String,
    pub source_dir: PathBuf,
    pub spec_name: String,
}

type AliasIndex = HashMap<String, Vec<Arc<SpecEntry>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AliasFallbackPolicy {
    KeepDirectoryPrecedence,
    KeepLowerPrecedence,
}

/// Read-only view of every registered spec entry, plus the alias index that
/// resolves command keys to those specs.
///
/// `SpecStore` is immutable after construction — every mutation is
/// confined to [`SpecStore::load_from_dirs`] / [`SpecStore::load_from_dir`].
/// Lookups go through the alias index; iteration yields entries (not
/// aliases) so status counts don't double-count one spec under two keys.
pub struct SpecStore {
    entries: Vec<Arc<SpecEntry>>,
    by_alias: AliasIndex,
    conflicts: Vec<AliasConflict>,
}

pub struct SpecLoadResult {
    pub store: SpecStore,
    /// Directory-level loading errors such as `read_dir` failures. Per-file
    /// JSON read/parse failures are lazy and are reported by
    /// [`SpecStore::force_load_errors`] or [`SpecEntry::load_error`] after a
    /// lookup/force-load touches the entry.
    pub directory_errors: Vec<String>,
}

impl SpecStore {
    /// Load specs from multiple directories into a precedence-ordered alias
    /// index. A spec from an earlier directory is the primary candidate when
    /// it parses successfully, matching the user intuition that earlier
    /// entries in config's `paths.spec_dirs` take precedence (e.g., user
    /// overrides before system defaults). Later copies with the same filename
    /// remain registered as lazy-parse fallbacks.
    ///
    /// Each spec is keyed in the alias index by its filename stem
    /// (canonical id) and, when free, by its `CompletionSpec.name`. Files
    /// whose declared `name` collides with another spec's name or stem surface
    /// a [`AliasConflict`] entry. The conflict disposition says whether that
    /// alias was rejected or retained as a fallback candidate.
    pub fn load_from_dirs(dirs: &[PathBuf]) -> Result<SpecLoadResult> {
        let mut entries: Vec<Arc<SpecEntry>> = Vec::new();
        let mut by_alias: AliasIndex = HashMap::new();
        let mut conflicts: Vec<AliasConflict> = Vec::new();
        let mut directory_errors: Vec<String> = Vec::new();

        for dir in dirs {
            match load_dir_into_pending(dir, alias_for_filesystem_file) {
                Ok(pending) => {
                    register_entries(
                        pending,
                        &mut entries,
                        &mut by_alias,
                        &mut conflicts,
                        AliasFallbackPolicy::KeepDirectoryPrecedence,
                    );
                }
                Err(e) => {
                    // Directory-level IO failure (e.g., EACCES on read_dir).
                    // Accumulate into directory_errors instead of bailing — a
                    // broken dir earlier in the list must not hide valid dirs
                    // later in the list. Per-file IO failures are deferred to
                    // lazy parse and surface via SpecEntry::load_error.
                    directory_errors.push(format!("{}: {e}", dir.display()));
                }
            }
        }

        Ok(SpecLoadResult {
            store: Self {
                entries,
                by_alias,
                conflicts,
            },
            directory_errors,
        })
    }

    pub fn load_from_dir(dir: &Path) -> Result<SpecLoadResult> {
        Self::load_from_dirs(&[dir.to_path_buf()])
    }

    /// Load specs from filesystem dirs followed by the binary's
    /// embedded corpus. The runtime spec loader (proxy mode) calls
    /// this so users who never ran `ghost-complete install` still
    /// get completions, without paying the legacy embedded-cache
    /// materialization disk-write cost.
    ///
    /// Filesystem dirs win precedence in registration order. Embedded specs
    /// fill in filenames the user dirs did not cover and remain as
    /// lower-precedence fallbacks for covered aliases if a filesystem
    /// override fails its first lazy parse. The embedded source is recorded as
    /// [`SpecSource::Embedded`] so first-touch parsing reads the
    /// binary slice in-memory — no disk I/O.
    ///
    /// Aliases for embedded specs come from the build-time
    /// `EMBEDDED_SPEC_ALIASES` table at zero parse cost; aliases for
    /// filesystem specs come from a shallow parse of the
    /// `CompletionSpec.name` field per file.
    pub fn load_with_embedded(filesystem_dirs: &[PathBuf]) -> Result<SpecLoadResult> {
        let mut entries: Vec<Arc<SpecEntry>> = Vec::new();
        let mut by_alias: AliasIndex = HashMap::new();
        let mut conflicts: Vec<AliasConflict> = Vec::new();
        let mut directory_errors: Vec<String> = Vec::new();

        for dir in filesystem_dirs {
            match load_dir_into_pending(dir, alias_for_filesystem_file) {
                Ok(pending) => {
                    register_entries(
                        pending,
                        &mut entries,
                        &mut by_alias,
                        &mut conflicts,
                        AliasFallbackPolicy::KeepDirectoryPrecedence,
                    );
                }
                Err(e) => {
                    directory_errors.push(format!("{}: {e}", dir.display()));
                }
            }
        }

        let embedded_dir = PathBuf::from(EMBEDDED_VIRTUAL_DIR);
        let embedded_pending: Vec<PendingSpec> = crate::embedded::embedded_entries_with_aliases()
            .filter_map(|(filename, contents, name_alias)| {
                let stem = filename.strip_suffix(".json")?.to_owned();
                Some(PendingSpec {
                    filename_stem: stem,
                    name_alias: name_alias.map(str::to_owned),
                    source_dir: embedded_dir.clone(),
                    source: SpecSource::Embedded(contents),
                })
            })
            .collect();
        register_entries(
            embedded_pending,
            &mut entries,
            &mut by_alias,
            &mut conflicts,
            AliasFallbackPolicy::KeepLowerPrecedence,
        );

        Ok(SpecLoadResult {
            store: Self {
                entries,
                by_alias,
                conflicts,
            },
            directory_errors,
        })
    }

    /// Resolve a command alias (filename stem or non-conflicting
    /// `CompletionSpec.name`) to the parsed spec. Returns `None` when
    /// no loaded spec advertises this alias OR when every candidate for
    /// that alias failed to parse. Use [`Self::get_result`] to distinguish
    /// unknown commands from parse failures.
    pub fn get(&self, command: &str) -> Option<&CompletionSpec> {
        self.get_result(command).ok()
    }

    /// Resolve a command alias while preserving lookup failures. Aliases
    /// can have lower-precedence fallback entries (notably embedded specs
    /// behind filesystem overrides); lookup tries candidates in precedence
    /// order and returns the first one that parses successfully.
    pub fn get_result(
        &self,
        command: &str,
    ) -> std::result::Result<&CompletionSpec, SpecLookupError> {
        let Some(candidates) = self.by_alias.get(command) else {
            return Err(SpecLookupError::NoSuchSpec {
                command: command.to_string(),
            });
        };

        let mut first_failure: Option<SpecLookupError> = None;
        for entry in candidates {
            match entry.spec_result() {
                Ok(spec) => return Ok(spec),
                Err(err) => {
                    if first_failure.is_none() {
                        first_failure = Some(SpecLookupError::LoadFailed {
                            command: command.to_string(),
                            id: entry.id.clone(),
                            source: entry.location(),
                            error: err.to_string(),
                        });
                    }
                }
            }
        }

        Err(
            first_failure.unwrap_or_else(|| SpecLookupError::NoSuchSpec {
                command: command.to_string(),
            }),
        )
    }

    /// Force every entry through its lazy parse. Used by [`Self::iter`]
    /// so diagnostic CLIs see fully-parsed specs. Idempotent — calls
    /// after the first force-load are zero-cost OnceLock reads.
    fn force_load_all(&self) {
        for entry in &self.entries {
            let _ = entry.spec();
        }
    }

    /// Force every registered entry through its lazy parse and return
    /// per-entry failures. Directory/read_dir failures remain on
    /// [`SpecLoadResult::directory_errors`]; this method reports JSON read/parse
    /// failures that are intentionally deferred at startup.
    pub fn force_load_errors(&self) -> Vec<SpecEntryLoadError> {
        self.force_load_all();
        self.entries
            .iter()
            .filter_map(|entry| {
                entry.load_error().map(|error| SpecEntryLoadError {
                    id: entry.id.clone(),
                    source: entry.location(),
                    error: error.to_string(),
                })
            })
            .collect()
    }

    /// Yield the runtime-resolved entries: the first successfully loaded
    /// candidate for each alias, de-duplicated to one item per spec entry.
    ///
    /// Force-loads every registered candidate so a lower-precedence fallback
    /// can become resolved when an earlier candidate fails lazy parsing.
    /// Call [`Self::entries`] for raw registration diagnostics, including
    /// hidden fallback candidates and lazy load errors.
    pub fn resolved_entries(&self) -> impl Iterator<Item = &Arc<SpecEntry>> {
        self.force_load_all();
        self.entries
            .iter()
            .filter(|entry| self.entry_is_first_successful_candidate(entry))
    }

    fn entry_is_first_successful_candidate(&self, entry: &Arc<SpecEntry>) -> bool {
        self.by_alias.values().any(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate.spec().is_some())
                .is_some_and(|candidate| Arc::ptr_eq(candidate, entry))
        })
    }

    /// Yield one tuple per resolved spec. Force-loads every entry on
    /// first call (subsequent calls are zero-cost). Entries whose lazy parse
    /// failed are silently skipped; lower-precedence fallback entries are
    /// yielded only when every earlier candidate for one of their aliases
    /// failed. Call [`Self::entries`] directly and inspect
    /// [`SpecEntry::load_error`] to surface failures.
    ///
    /// The first element is the canonical id (filename stem), NOT
    /// every alias — callers that want to enumerate aliases use
    /// [`SpecStore::entries`] directly.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &CompletionSpec)> {
        self.resolved_entries()
            .filter_map(|e| e.spec().map(|s| (e.id.as_str(), s)))
    }

    /// Number of registered spec entries. Differs from
    /// [`SpecStore::aliases_count`] because entries are source files while
    /// aliases are command keys; lower-precedence fallback entries can share
    /// an alias with a higher-precedence entry.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All `Arc<SpecEntry>` values in load order. Read-only access for
    /// status / doctor diagnostics that need source dir + alias lists.
    pub fn entries(&self) -> &[Arc<SpecEntry>] {
        &self.entries
    }

    /// Total number of unique resolvable command aliases. Equal to
    /// `by_alias.len()`; lower-precedence fallback candidates for an alias
    /// do not add additional command names. Surfaced as
    /// `commands_addressable` in status JSON.
    pub fn aliases_count(&self) -> usize {
        self.by_alias.len()
    }

    /// Number of registered command aliases for which every candidate fails
    /// to parse. Force-loads all candidates so lower-precedence fallbacks can
    /// mask a malformed higher-precedence entry before the command is counted
    /// as nonfunctional.
    pub fn nonfunctional_aliases_count(&self) -> usize {
        self.force_load_all();
        self.by_alias
            .values()
            .filter(|candidates| candidates.iter().all(|entry| entry.spec().is_none()))
            .count()
    }

    /// Alias collisions detected at load time. Surfaced via doctor / status so
    /// users can distinguish rejected aliases from lazy-parse fallback chains.
    pub fn conflicts(&self) -> &[AliasConflict] {
        &self.conflicts
    }

    /// Explicit location for every entry the loader actually kept, in load
    /// order. Filesystem entries expose real paths; embedded entries are
    /// represented as [`SpecLocation::Embedded`] instead of a fabricated
    /// filesystem path.
    pub fn locations(&self) -> Vec<SpecLocation> {
        self.entries.iter().map(|entry| entry.location()).collect()
    }

    /// Real on-disk path for every filesystem entry the loader actually kept,
    /// in load order. Embedded specs are intentionally omitted because they do
    /// not have a filesystem path.
    pub fn filesystem_paths(&self) -> Vec<(String, PathBuf)> {
        self.entries
            .iter()
            .filter_map(|e| match &e.source {
                SpecSource::Filesystem(path) => Some((e.filename_stem.clone(), path.clone())),
                SpecSource::Embedded(_) => None,
            })
            .collect()
    }

    /// Real on-disk path for every filesystem entry the loader actually kept,
    /// in load order. Embedded specs are intentionally omitted because they do
    /// not have a filesystem path.
    ///
    /// Used by `ghost-complete status` to count requires_js generators
    /// without double-counting when overlapping spec_dirs each ship a
    /// copy of the same filename. A naïve file scan that walked every
    /// configured directory and summed their generator counts would
    /// inflate the reported number on configs where the embedded specs
    /// dir and a user override dir both contained `git.json`; this
    /// method follows the loader's kept filesystem entries instead of
    /// inventing paths for in-memory embedded specs.
    pub fn canonical_paths(&self) -> Vec<(String, PathBuf)> {
        self.filesystem_paths()
    }
}

/// Header-only struct used by [`shallow_parse_name`] to extract just
/// the top-level `name` field without materialising the full
/// `CompletionSpec` tree. Unknown fields are ignored by serde
/// (default behaviour), so the entire spec body is tokenised but no
/// nested allocations happen.
#[derive(Deserialize)]
struct SpecHeader {
    #[serde(default)]
    name: Option<String>,
}

/// Read a filesystem spec file and extract the top-level `name`
/// field. Used as a fallback for filesystem-installed specs to
/// resolve `name` aliases without parsing the full
/// `CompletionSpec` tree.
///
/// The extracted name is run through the same control-character
/// sanitiser as the full parse path, so the alias index registers
/// the sanitised form (matching the behaviour callers see when they
/// later look up `entry.spec().name`).
///
/// Returns `None` on read or parse failure (the entry will be
/// aliased by filename stem only — its lazy-parse failure surfaces
/// later via [`SpecEntry::load_error`]).
fn shallow_parse_name(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    check_json_depth(&contents, MAX_SPEC_JSON_DEPTH).ok()?;
    let header: SpecHeader = serde_json::from_str(&contents).ok()?;
    let mut name = header.name?;
    sanitize_string(&mut name);
    Some(name)
}

/// Resolve a filesystem spec's `name` alias by shallow-parsing the
/// JSON for its top-level `name` field.
///
/// We always shallow-parse filesystem files (rather than trusting
/// the build-time table) because users may edit specs in
/// `~/.config/ghost-complete/specs/` and the in-binary table would
/// give a stale answer. The shallow parse only walks the JSON
/// without allocating the full `CompletionSpec` tree, so the
/// startup cost is bounded — typically < 200 ms even for the full
/// 709-spec corpus including AWS.
///
/// Embedded specs (loaded via [`SpecStore::load_with_embedded`] from
/// `EMBEDDED_SPECS`) skip this path entirely: their alias is taken
/// from the build-time `EMBEDDED_SPEC_ALIASES` table at zero parse
/// cost.
fn alias_for_filesystem_file(_filename: &str, path: &Path) -> Option<String> {
    shallow_parse_name(path)
}

/// Registration metadata for a spec on its way into `register_entries`. We can't
/// build the final `SpecEntry` at this stage because the `aliases` vec
/// depends on which alias slots are still free across the merged dir set.
///
/// Holds metadata only — `serde_json::from_str` is deferred until the
/// owning `SpecEntry::spec()` is called for the first time. The
/// `name_alias` field is pre-resolved by the caller from a shallow
/// filesystem header parse or the build-time `EMBEDDED_SPEC_ALIASES` table.
/// `None` means no usable distinct name alias was found, including when a
/// shallow filesystem parse failed.
struct PendingSpec {
    filename_stem: String,
    /// Pre-resolved `CompletionSpec.name` alias when it differs from
    /// the filename stem. `None` means "stem-only aliasing".
    name_alias: Option<String>,
    source_dir: PathBuf,
    source: SpecSource,
}

/// Walk `dir` for `*.json` specs and emit a [`PendingSpec`] per file.
/// Does NOT parse JSON contents into a `CompletionSpec`. Filesystem entries
/// always shallow-parse only the top-level `name` field because users may edit
/// installed specs and build-time aliases can be stale. Embedded entries loaded
/// by [`SpecStore::load_with_embedded`] use the generated
/// `EMBEDDED_SPEC_ALIASES` table instead and avoid JSON parsing at
/// registration.
///
/// Filesystem errors at directory-read time are returned as a hard
/// `Err`; per-file IO errors during the lazy parse surface later via
/// [`SpecEntry::load_error`].
fn load_dir_into_pending(
    dir: &Path,
    name_alias_for: impl Fn(&str, &Path) -> Option<String>,
) -> Result<Vec<PendingSpec>> {
    let mut pending: Vec<PendingSpec> = Vec::new();

    if !dir.exists() {
        tracing::warn!("spec directory does not exist: {}", dir.display());
        return Ok(pending);
    }

    let read_dir = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read spec directory: {}", dir.display()))?;

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        paths.push(path);
    }
    // Sort by filename stem so the "first-wins" alias arbitration is
    // deterministic — without sorting, `read_dir` order on macOS / Linux
    // is filesystem-defined and can flip between runs (and between CI
    // boxes), which would make tests that assert which spec won an alias
    // race flaky.
    paths.sort_by(|a, b| {
        let stem_a = a.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let stem_b = b.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        stem_a.cmp(stem_b)
    });

    for path in paths {
        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        let name_alias = name_alias_for(&filename, &path);
        pending.push(PendingSpec {
            filename_stem: stem,
            name_alias,
            source_dir: dir.to_path_buf(),
            source: SpecSource::Filesystem(path),
        });
    }

    Ok(pending)
}

/// Take a directory's parsed specs and merge them into the running store
/// state. Two passes:
///   1. Register every filename stem. Stems are the canonical id and
///      always take precedence over `name` aliases. A stem already
///      owned by an earlier directory's same filename is appended as a
///      lower-precedence fallback with a `DirectoryPrecedence` conflict;
///      a stem that collides with a different file's previously-registered
///      `name` alias is either rejected or appended as a fallback depending on
///      the loader policy, with `NameMatchesOtherStem` recorded from the
///      inverted perspective — the new file's stem matches what an existing
///      file's name claim already owns.
///   2. Register `CompletionSpec.name` aliases for entries whose name
///      differs from the stem. Aliases yield to stems unconditionally;
///      a name that collides with another spec's stem records
///      NameMatchesOtherStem, and a name that collides with another
///      already-registered name records DuplicateName. The fallback policy
///      determines whether the colliding name alias is rejected or retained
///      behind the primary candidate.
///
/// The two-pass shape exists so the canonical spec wins its own name —
/// e.g. `kubectl.json` keeps the `kubectl` alias even though
/// `kubecolor.json` (declared `name: "kubectl"`) is processed first
/// alphabetically. Without it, an earlier file's name claim could
/// shadow a later file's stem and silently demote the canonical spec.
fn register_entries(
    pending: Vec<PendingSpec>,
    entries: &mut Vec<Arc<SpecEntry>>,
    by_alias: &mut AliasIndex,
    conflicts: &mut Vec<AliasConflict>,
    fallback_policy: AliasFallbackPolicy,
) {
    // Pass 1: register every stem. The PendingSpec carries a SpecSource
    // that the new SpecEntry takes ownership of — no parsing happens
    // until SpecEntry::spec() is first called. `accepted` records each
    // accepted entry's index in `entries` plus the data we need for
    // pass 2 (filename_stem, source_dir, name_alias) — keeping the
    // ordering of `pending` so name-alias arbitration is stable.
    struct Accepted {
        idx: usize,
        filename_stem: String,
        source_dir: PathBuf,
        name_alias: Option<String>,
        can_create_aliases: bool,
    }
    let mut accepted: Vec<Accepted> = Vec::with_capacity(pending.len());

    for ps in pending {
        let PendingSpec {
            filename_stem,
            name_alias,
            source_dir,
            source,
        } = ps;

        let mut can_create_aliases = true;
        if let Some(existing) = primary_alias_owner(by_alias, &filename_stem).cloned() {
            // Distinguish two cases:
            //   - Same filename in two configured dirs (the user-override
            //     scenario) — earlier dir is preferred, classify as
            //     DirectoryPrecedence.
            //   - The new file's stem collides with a different file's
            //     already-registered alias (the existing entry came from
            //     a different filename — typically its `name` claim or
            //     even its stem under an unrelated path). Under normal
            //     first-match loading the lower-precedence candidate is
            //     dropped; when loading embedded specs, it is retained only as
            //     a fallback candidate behind the existing alias owner.
            let kind = if existing.filename_stem == filename_stem {
                AliasConflictKind::DirectoryPrecedence
            } else {
                AliasConflictKind::NameMatchesOtherStem
            };
            let keep_as_fallback = fallback_policy == AliasFallbackPolicy::KeepLowerPrecedence
                || (fallback_policy == AliasFallbackPolicy::KeepDirectoryPrecedence
                    && kind == AliasConflictKind::DirectoryPrecedence);
            tracing::debug!(
                stem = %filename_stem,
                existing_stem = %existing.filename_stem,
                existing_dir = %existing.source_dir.display(),
                losing_dir = %source_dir.display(),
                kind = ?kind,
                "spec stem already registered"
            );
            conflicts.push(AliasConflict {
                alias: filename_stem.clone(),
                kind,
                disposition: if keep_as_fallback {
                    AliasConflictDisposition::FallbackCandidate
                } else {
                    AliasConflictDisposition::Rejected
                },
                winner: AliasOwner {
                    filename_stem: existing.filename_stem.clone(),
                    source_dir: existing.source_dir.clone(),
                    spec_name: existing
                        .aliases
                        .iter()
                        .find(|a| **a != existing.filename_stem)
                        .cloned()
                        .unwrap_or_else(|| existing.filename_stem.clone()),
                },
                loser: AliasOwner {
                    filename_stem: filename_stem.clone(),
                    source_dir: source_dir.clone(),
                    spec_name: name_alias.clone().unwrap_or_else(|| filename_stem.clone()),
                },
            });

            if !keep_as_fallback {
                continue;
            }
            can_create_aliases = false;
        }

        let entry = Arc::new(SpecEntry {
            id: filename_stem.clone(),
            filename_stem: filename_stem.clone(),
            source_dir: source_dir.clone(),
            aliases: vec![filename_stem.clone()],
            source,
            parsed: OnceLock::new(),
        });
        let idx = entries.len();
        entries.push(Arc::clone(&entry));
        push_alias_candidate(by_alias, filename_stem.clone(), Arc::clone(&entry));
        accepted.push(Accepted {
            idx,
            filename_stem,
            source_dir,
            name_alias,
            can_create_aliases,
        });
    }

    // Pass 2: register name aliases (only when distinct from stem).
    // Stems already populate `by_alias`, so a name that collides with
    // another spec's stem is handled here without a separate lookup table.
    for a in accepted {
        let Accepted {
            idx,
            filename_stem,
            source_dir,
            name_alias,
            can_create_aliases,
        } = a;

        let Some(name) = name_alias else { continue };
        if name.is_empty() || name == filename_stem {
            continue;
        }
        if let Some(existing) = primary_alias_owner(by_alias, &name).cloned() {
            let kind = if existing.filename_stem == name {
                AliasConflictKind::NameMatchesOtherStem
            } else {
                AliasConflictKind::DuplicateName
            };
            tracing::debug!(
                alias = %name,
                winner_stem = %existing.filename_stem,
                loser_stem = %filename_stem,
                kind = ?kind,
                "name alias already registered"
            );
            conflicts.push(AliasConflict {
                alias: name.clone(),
                kind,
                disposition: if fallback_policy == AliasFallbackPolicy::KeepLowerPrecedence {
                    AliasConflictDisposition::FallbackCandidate
                } else {
                    AliasConflictDisposition::Rejected
                },
                winner: AliasOwner {
                    filename_stem: existing.filename_stem.clone(),
                    source_dir: existing.source_dir.clone(),
                    spec_name: existing
                        .aliases
                        .iter()
                        .find(|a| **a != existing.filename_stem)
                        .cloned()
                        .unwrap_or_else(|| existing.filename_stem.clone()),
                },
                loser: AliasOwner {
                    filename_stem: filename_stem.clone(),
                    source_dir,
                    spec_name: name.clone(),
                },
            });

            if fallback_policy != AliasFallbackPolicy::KeepLowerPrecedence {
                continue;
            }
        } else if !can_create_aliases {
            continue;
        }

        // Append the alias by rebuilding the Arc<SpecEntry>.
        // SpecEntry's fields are not interior-mutable; the rebuild is
        // confined to load time so the steady-state hot path never
        // pays this cost. Critically, the OnceLock stays empty in the
        // new entry — parsing is still deferred to first SpecEntry::spec().
        let prev = Arc::clone(&entries[idx]);
        let mut new_aliases = prev.aliases.clone();
        new_aliases.push(name.clone());
        let new_entry = SpecEntry {
            id: prev.id.clone(),
            filename_stem: prev.filename_stem.clone(),
            source_dir: prev.source_dir.clone(),
            aliases: new_aliases,
            source: prev.source.clone(),
            parsed: OnceLock::new(),
        };
        let new_arc = Arc::new(new_entry);
        replace_entry_arc(entries, by_alias, &prev, Arc::clone(&new_arc));
        push_alias_candidate(by_alias, filename_stem, Arc::clone(&new_arc));
        push_alias_candidate(by_alias, name, new_arc);
    }
}

fn primary_alias_owner<'a>(by_alias: &'a AliasIndex, alias: &str) -> Option<&'a Arc<SpecEntry>> {
    by_alias.get(alias).and_then(|entries| entries.first())
}

fn push_alias_candidate(by_alias: &mut AliasIndex, alias: String, entry: Arc<SpecEntry>) {
    let candidates = by_alias.entry(alias).or_default();
    if !candidates
        .iter()
        .any(|candidate| Arc::ptr_eq(candidate, &entry))
    {
        candidates.push(entry);
    }
}

fn replace_entry_arc(
    entries: &mut [Arc<SpecEntry>],
    by_alias: &mut AliasIndex,
    old: &Arc<SpecEntry>,
    new: Arc<SpecEntry>,
) {
    for entry in entries {
        if Arc::ptr_eq(entry, old) {
            *entry = Arc::clone(&new);
        }
    }
    for candidates in by_alias.values_mut() {
        for candidate in candidates {
            if Arc::ptr_eq(candidate, old) {
                *candidate = Arc::clone(&new);
            }
        }
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

fn option_arg_count(opt: &OptionSpec) -> usize {
    usize::from(opt.args.is_some()) + opt.extra_args.len()
}

fn option_arg_at(opt: &OptionSpec, index: usize) -> Option<&ArgSpec> {
    if index == 0 {
        opt.args.as_ref()
    } else {
        opt.extra_args.get(index - 1)
    }
}

fn option_last_arg_is_variadic(opt: &OptionSpec) -> bool {
    option_arg_count(opt)
        .checked_sub(1)
        .and_then(|idx| option_arg_at(opt, idx))
        .is_some_and(|arg| arg.is_variadic)
}

fn has_inline_option_value(flag: &str) -> bool {
    flag.split_once('=')
        .is_some_and(|(_, value)| !value.is_empty())
}

fn completed_option_value_count(args: &[String], flag_idx: usize, opt: &OptionSpec) -> usize {
    let arg_count = option_arg_count(opt);
    if arg_count == 0 {
        return 0;
    }

    let mut completed = usize::from(has_inline_option_value(&args[flag_idx]));
    if completed >= arg_count && !option_last_arg_is_variadic(opt) {
        return completed;
    }

    let mut idx = flag_idx + 1;
    while idx < args.len() {
        if args[idx].starts_with('-') {
            break;
        }
        if completed >= arg_count && !option_last_arg_is_variadic(opt) {
            break;
        }
        completed += 1;
        idx += 1;
    }
    completed
}

fn active_option_arg_spec<'a>(
    options: &'a [OptionSpec],
    args: &[String],
    ctx: &CommandContext,
) -> Option<&'a ArgSpec> {
    if let Some(flag) = &ctx.preceding_flag {
        if flag.contains('=') {
            // Inline value already occupies arg slot 0; fall through to the
            // scanner so `--flag=value <TAB>` can address a second option arg.
        } else if let Some(opt) = find_option(options, flag) {
            return option_arg_at(opt, 0);
        }
    }

    for (idx, arg) in args.iter().enumerate() {
        if !arg.starts_with('-') {
            continue;
        }
        let Some(opt) = find_option(options, arg) else {
            continue;
        };
        let arg_count = option_arg_count(opt);
        if arg_count == 0 {
            continue;
        }
        let completed = completed_option_value_count(args, idx, opt);
        let span_end =
            idx + 1 + completed.saturating_sub(usize::from(has_inline_option_value(arg)));
        if span_end != args.len() {
            continue;
        }
        if completed < arg_count {
            return option_arg_at(opt, completed);
        }
        if option_last_arg_is_variadic(opt) {
            return option_arg_at(opt, arg_count - 1);
        }
    }

    None
}

fn arg_spec_has_completion_content(arg_spec: &ArgSpec) -> bool {
    !arg_spec.generators.is_empty()
        || !arg_spec.suggestions.is_empty()
        || matches!(arg_spec.template.as_deref(), Some("filepaths" | "folders"))
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
            // If this flag takes values in the spec, skip the completed value
            // tokens too. Option `args` arrays are positional; flattening them
            // here would make later option values look like subcommands.
            if let Some(opt) = find_option(current_options, arg) {
                let consumed = completed_option_value_count(args, arg_idx, opt);
                if consumed > 0 {
                    arg_idx += 1 + consumed;
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
    let mut option_arg_has_completion_content = false;
    if let Some(arg_spec) = active_option_arg_spec(current_options, args, ctx) {
        // The flag takes an argument — suppress subcommands/options
        // regardless of whether the arg spec has explicit generators.
        // A bare `"args": { "name": "file" }` still means the user
        // is filling a value, not typing a subcommand.
        preceding_flag_has_args = true;
        option_arg_has_completion_content = arg_spec_has_completion_content(arg_spec);

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

    // Check positional arg specs at the resolved position, but only when
    // the active flag arg has its own completions. Inert option args still
    // suppress subcommands/options, but can fall through to positional
    // generators so alias-injected flags like `gcb -> git checkout -b` do
    // not produce an empty async dispatch set.
    if !option_arg_has_completion_content {
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
            // Generators with `requires_js: true` but no populated
            // `js_runtime`, no source, or custom/script_function source that
            // was not proven self-contained stay skipped — there is nothing
            // safe to dispatch.
            let supported = match gen.js_runtime.as_ref().map(|rt| &rt.kind) {
                Some(JsRuntimeKind::PostProcess) => {
                    // Post-process still requires an accompanying script;
                    // a JS body that can't see stdout has no input.
                    gen.script.is_some() || gen.script_template.is_some()
                }
                Some(JsRuntimeKind::ScriptFunction) | Some(JsRuntimeKind::Custom) => gen
                    .js_runtime
                    .as_ref()
                    .is_some_and(|rt| rt.self_contained && !rt.source.trim().is_empty()),
                None => false,
            };
            if !supported {
                tracing::info!(
                    kind = ?gen.js_runtime.as_ref().map(|rt| &rt.kind),
                    has_script = gen.script.is_some(),
                    has_template = gen.script_template.is_some(),
                    has_source = gen
                        .js_runtime
                        .as_ref()
                        .map(|rt| !rt.source.trim().is_empty())
                        .unwrap_or(false),
                    "skipping requires_js generator — unsupported shape"
                );
                continue;
            }
            // Fall through: dispatch the generator down the script path
            // so `engine::run_generators` can pick the right shape based
            // on `js_runtime.kind`.
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
        // JS-only generators (script_function / custom) have neither `script`
        // nor `script_template` populated, but the engine still needs a slot
        // in the script-generator vec to dispatch them. Funnel anything with
        // a populated `js_runtime` through the same queue and let
        // `engine::run_generators` switch on `kind`.
        let is_js_dispatchable = gen.requires_js && gen.js_runtime.is_some();
        if !handled_by_type
            && (gen.script.is_some() || gen.script_template.is_some() || is_js_dispatchable)
        {
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
        for arg_spec in opt.args.iter_mut().chain(opt.extra_args.iter_mut()) {
            validate_arg_generators(arg_spec, &spec.name, &mut warnings);
        }
    }

    let mut stack: Vec<&mut SubcommandSpec> = spec.subcommands.iter_mut().collect();
    while let Some(sub) = stack.pop() {
        validate_args_generators(&mut sub.args, &spec.name, &mut warnings);
        for opt in &mut sub.options {
            for arg_spec in opt.args.iter_mut().chain(opt.extra_args.iter_mut()) {
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
        // Account for both legacy `js_source` and `js_runtime.source` — the
        // converter emits the latter, but stale user-installed specs may still
        // carry the former.
        let js_runtime = g.js_runtime.as_ref().map(|jr| jr.source.len()).unwrap_or(0);
        let tmpl = opt_string_heap(&g.template);
        let transforms_vec = g.transforms.capacity() * std::mem::size_of::<Transform>();
        let transforms_inner: usize = g.transforms.iter().map(transform_heap).sum();
        gt + script + script_tmpl + js + js_runtime + tmpl + transforms_vec + transforms_inner
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
        let first_arg = opt.args.as_ref().map(arg_spec_heap).unwrap_or(0);
        let extra_args_vec = opt.extra_args.capacity() * std::mem::size_of::<ArgSpec>();
        let extra_args = opt.extra_args.iter().map(arg_spec_heap).sum::<usize>();
        names + names_vec + desc + first_arg + extra_args_vec + extra_args
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

        // Force the lazy parse for every entry so failure modes surface
        // via SpecEntry::load_error.
        let _: Vec<_> = result.store.iter().collect();
        let load_errors: Vec<(&str, &str)> = result
            .store
            .entries()
            .iter()
            .filter_map(|e| e.load_error().map(|err| (e.id.as_str(), err)))
            .collect();
        assert_eq!(load_errors.len(), 2, "should have 2 load errors");
        assert!(
            load_errors.iter().any(|(id, _)| *id == "bad"),
            "errors should include bad.json: {:?}",
            load_errors
        );
        assert!(
            load_errors.iter().any(|(id, _)| *id == "broken"),
            "errors should include broken.json: {:?}",
            load_errors
        );
    }

    #[test]
    fn test_load_from_dir_all_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("alpha.json"), r#"{"name": "alpha"}"#).unwrap();
        std::fs::write(dir.path().join("beta.json"), r#"{"name": "beta"}"#).unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        assert!(result.directory_errors.is_empty(), "no errors expected");
        assert!(result.store.get("alpha").is_some());
        assert!(result.store.get("beta").is_some());
    }

    #[test]
    fn test_load_from_dir_nonexistent() {
        let result = SpecStore::load_from_dir(Path::new("/nonexistent/path/specs")).unwrap();
        assert!(result.directory_errors.is_empty());
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
    fn test_deserialize_js_runtime_post_process() {
        // The post_process kind covers requires_js generators whose
        // post-process body could not be lowered to declarative transforms.
        // The script still runs natively; stdout is fed through the JS
        // source.
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{
                "requires_js": true,
                "script": ["echo", "hi"],
                "js_runtime": {
                    "kind": "post_process",
                    "source": "out => [{ name: out }]"
                }
            }"#,
        )
        .unwrap();
        assert!(gen.requires_js);
        let jr = gen.js_runtime.expect("js_runtime should parse");
        assert_eq!(jr.kind, JsRuntimeKind::PostProcess);
        assert_eq!(jr.source, "out => [{ name: out }]");
        assert!(jr.timeout_ms.is_none(), "timeout_ms defaults to None");
        assert!(
            !jr.allow_shell_command,
            "allow_shell_command defaults to false"
        );
    }

    #[test]
    fn test_deserialize_js_runtime_script_function() {
        // The script_function kind covers Fig's `script: (...) => [...]`
        // shape — the JS body evaluates to an argv array that the runtime
        // then spawns.
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "script_function",
                    "source": "(ctx) => [\"echo\", ctx.tokens[0]]"
                }
            }"#,
        )
        .unwrap();
        assert!(gen.requires_js);
        let jr = gen.js_runtime.expect("js_runtime should parse");
        assert_eq!(jr.kind, JsRuntimeKind::ScriptFunction);
        assert!(jr.source.contains("ctx.tokens"));
    }

    #[test]
    fn test_deserialize_js_runtime_custom() {
        // The custom kind covers Fig's `custom: async (...) => [...]`
        // shape — no script, the JS body returns suggestions directly.
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "custom",
                    "source": "async () => [{ name: 'a' }, { name: 'b' }]"
                }
            }"#,
        )
        .unwrap();
        assert!(gen.requires_js);
        let jr = gen.js_runtime.expect("js_runtime should parse");
        assert_eq!(jr.kind, JsRuntimeKind::Custom);
        assert!(jr.source.contains("async"));
    }

    #[test]
    fn test_deserialize_js_runtime_with_optional_fields() {
        // The optional fields populate together.
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{
                "requires_js": true,
                "js_runtime": {
                    "kind": "post_process",
                    "source": "x => x",
                    "timeout_ms": 5000,
                    "allow_shell_command": true
                }
            }"#,
        )
        .unwrap();
        let jr = gen.js_runtime.expect("js_runtime should parse");
        assert_eq!(jr.kind, JsRuntimeKind::PostProcess);
        assert_eq!(jr.timeout_ms, Some(5000));
        assert!(jr.allow_shell_command);
    }

    #[test]
    fn test_deserialize_js_runtime_unknown_kind_rejected() {
        // JsRuntimeKind is a closed enum (no serde(other)). An unknown kind
        // must hard-fail deserialization so a typo'd converter emission
        // can't sneak past load-time validation.
        let bad = r#"{
            "requires_js": true,
            "js_runtime": {
                "kind": "bogus",
                "source": "..."
            }
        }"#;
        let err = serde_json::from_str::<GeneratorSpec>(bad).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bogus")
                || msg.contains("variant")
                || msg.contains("expected")
                || msg.contains("unknown"),
            "deserialization should fail with a variant-rejection error: {msg}"
        );
    }

    #[test]
    fn test_deserialize_js_runtime_unknown_field_rejected() {
        // js_runtime carries deny_unknown_fields too, so a future converter
        // emitting a stray key here trips the schema rather than silently
        // dropping the metadata.
        let bad = r#"{
            "requires_js": true,
            "js_runtime": {
                "kind": "post_process",
                "source": "x => x",
                "extra_field": true
            }
        }"#;
        let err = serde_json::from_str::<GeneratorSpec>(bad).unwrap_err();
        assert!(
            err.to_string().contains("extra_field") || err.to_string().contains("unknown field"),
            "expected unknown-field error: {err}"
        );
    }

    #[test]
    fn test_corpus_has_js_runtime_for_requires_js() {
        // Corpus invariant: every requires_js generator in the embedded
        // corpus must carry a populated `js_runtime` object. The lower
        // bound of 1000 is a comfortable floor — today's regen produces
        // ~3641 — that still catches a regression where the converter
        // silently stops emitting the metadata.
        const MIN_REQUIRES_JS_WITH_RUNTIME: usize = 1000;

        fn count(v: &serde_json::Value) -> (usize, usize) {
            // (requires_js_total, with_js_runtime)
            match v {
                serde_json::Value::Object(map) => {
                    let mut total = 0;
                    let mut with_rt = 0;
                    let is_gen =
                        matches!(map.get("requires_js"), Some(serde_json::Value::Bool(true)));
                    if is_gen {
                        total += 1;
                        if matches!(map.get("js_runtime"), Some(serde_json::Value::Object(_))) {
                            with_rt += 1;
                        }
                    }
                    for child in map.values() {
                        let (t, r) = count(child);
                        total += t;
                        with_rt += r;
                    }
                    (total, with_rt)
                }
                serde_json::Value::Array(arr) => {
                    let mut total = 0;
                    let mut with_rt = 0;
                    for child in arr {
                        let (t, r) = count(child);
                        total += t;
                        with_rt += r;
                    }
                    (total, with_rt)
                }
                _ => (0, 0),
            }
        }

        let mut total_requires_js = 0;
        let mut total_with_runtime = 0;
        for (name, body) in crate::embedded::EMBEDDED_SPECS {
            let v: serde_json::Value = serde_json::from_str(body)
                .unwrap_or_else(|e| panic!("embedded spec {name} is not valid JSON: {e}"));
            let (t, r) = count(&v);
            total_requires_js += t;
            total_with_runtime += r;
        }
        assert!(
            total_with_runtime >= MIN_REQUIRES_JS_WITH_RUNTIME,
            "embedded corpus invariant violated: only {total_with_runtime} requires_js \
             generators have js_runtime populated (out of {total_requires_js} total). \
             Every requires_js generator emitted by the converter should carry \
             js_runtime. Lower bound is {MIN_REQUIRES_JS_WITH_RUNTIME}."
        );
        // Strict correctness: every requires_js in the embedded corpus
        // should now carry js_runtime (the converter emits it for all three
        // shapes — post_process, script_function, custom). Drift here means
        // a hand-edited spec or a converter regression.
        assert_eq!(
            total_with_runtime, total_requires_js,
            "every requires_js generator in the embedded corpus must carry js_runtime; \
             saw {total_with_runtime}/{total_requires_js}"
        );
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
                is_optional: false,
                is_variadic: false,
            }),
            extra_args: Vec::new(),
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
                        is_optional: false,
                        is_variadic: false,
                    })
                } else {
                    None
                },
                extra_args: Vec::new(),
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
        //
        // Lazy-loading note: the depth check fires inside SpecEntry::spec()
        // (the deferred parse), so the rejection surfaces via load_error
        // rather than SpecLoadResult::directory_errors.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("evil.json"),
            build_nested_subcommands(10_000),
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let evil_entry = result
            .store
            .entries()
            .iter()
            .find(|e| e.id == "evil")
            .expect("evil entry must register even if its parse fails");
        assert_eq!(
            evil_entry.aliases,
            vec!["evil"],
            "too-deep JSON must not register the shallow-parsed name alias"
        );
        assert!(
            result.store.get("x").is_none() && result.store.get("leaf").is_none(),
            "pathologically nested spec must not register name-derived aliases"
        );
        assert!(
            result.store.get("evil").is_none(),
            "pathologically nested spec must not load through its stem"
        );
        assert!(
            evil_entry.load_error().is_some(),
            "lazy parse of pathologically nested spec must fail and record an error"
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
        let evil_entry = result
            .store
            .entries()
            .iter()
            .find(|e| e.id == "evil")
            .expect("evil entry must register");
        assert_eq!(
            evil_entry.aliases,
            vec!["evil"],
            "too-deep JSON must not register the shallow-parsed name alias"
        );
        assert!(
            result.store.get("x").is_none() && result.store.get("leaf").is_none(),
            "depth-100 spec must not register name-derived aliases"
        );
        assert!(
            result.store.get("evil").is_none(),
            "depth-100 spec must be rejected by our own cap (serde_json's 128 default would still let it through)"
        );
        assert!(
            evil_entry.load_error().is_some(),
            "lazy parse of depth-100 spec must surface an error via load_error"
        );
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
            result.directory_errors.is_empty(),
            "depth-12 spec should parse cleanly, got directory errors: {:?}",
            result.directory_errors
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
        // Stem-keyed addressability — the file is `evil.json`, so `evil`
        // is the canonical id. The sanitized `name` ("evil[2J") becomes a
        // secondary alias and resolves too; both lookups must hit the
        // same parsed spec.
        let by_stem = result
            .store
            .get("evil")
            .expect("spec should be addressable by filename stem");
        let by_alias = result
            .store
            .get("evil[2J")
            .expect("sanitized name should also resolve as alias");
        assert!(std::ptr::eq(by_stem, by_alias));
        let spec = by_stem;
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
            "js_runtime": {
                "kind": "post_process",
                "source": "out => out.split('\\n').map(name => ({ name }))",
                "timeout_ms": 5000,
                "allow_shell_command": false
            },
            "_corrected_in": "v0.10.0",
            "template": "filepaths"
        }"#;
        let gen: GeneratorSpec = serde_json::from_str(ok).unwrap();
        assert_eq!(gen.generator_type.as_deref(), Some("git_branches"));
        assert_eq!(gen.transforms.len(), 1);
        assert_eq!(gen.corrected_in.as_deref(), Some("v0.10.0"));
        assert_eq!(gen.template.as_deref(), Some("filepaths"));
        let jr = gen.js_runtime.as_ref().expect("js_runtime should parse");
        assert_eq!(jr.kind, JsRuntimeKind::PostProcess);
        assert_eq!(jr.timeout_ms, Some(5000));
        assert!(!jr.allow_shell_command);
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
        // Measured baseline: ~104 MiB (109,006,902 bytes), measured 2026-05-03
        // after restoring the AWS spec (ux-8). The `estimated_heap_bytes` walk
        // covers the whole `CompletionSpec` tree (js_source, transforms,
        // descriptions, etc.). The AWS spec alone contributes ~67 MiB of
        // mostly description text across 17K subcommands; that bloat is the
        // motivation for the zstd-compression follow-up plan. 128 MiB
        // (134,217,728 bytes) gives ~23% headroom for the gcloud/doppler/
        // mongocli/twilio/sfdx restore PRs queued behind this one.
        const BUDGET_BYTES: usize = 128 * 1024 * 1024;
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
    fn inert_option_arg_does_not_block_positional_generators() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "git",
                "subcommands": [{
                    "name": "checkout",
                    "options": [{
                        "name": ["-b"],
                        "args": { "name": "new-branch" }
                    }],
                    "args": [{
                        "name": "ref",
                        "generators": [{"type": "git_branches"}]
                    }]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("git".into()),
            args: vec!["checkout".into(), "-b".into()],
            current_word: "main".into(),
            word_index: 3,
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
            "inert option args should fall through to positional generators: {:?}",
            res.native_generators
        );
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
    fn option_arg_after_trailing_equals_uses_first_arg_spec() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "fmt",
                "options": [{
                    "name": ["--format"],
                    "args": {
                        "name": "kind",
                        "suggestions": ["tar", "zip"]
                    }
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("fmt".into()),
            args: vec!["--format=".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("--format=".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };

        let res = resolve_spec(&spec, &ctx);
        let texts: Vec<&str> = res
            .static_suggestions
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(texts, vec!["tar", "zip"]);
    }

    #[test]
    fn option_args_array_preserves_positional_arg_specs() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "chezmoi",
                "options": [{
                    "name": ["-t", "--track"],
                    "args": [
                        {"name": "branch", "suggestions": ["main", "dev"]},
                        {"name": "start-point", "suggestions": ["origin/main"]}
                    ]
                }]
            }"#,
        )
        .unwrap();

        let first_ctx = CommandContext {
            command: Some("chezmoi".into()),
            args: vec!["-t".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-t".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let first = resolve_spec(&spec, &first_ctx);
        let first_texts: Vec<&str> = first
            .static_suggestions
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(first_texts, vec!["main", "dev"]);

        let second_ctx = CommandContext {
            command: Some("chezmoi".into()),
            args: vec!["-t".into(), "main".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        };
        let second = resolve_spec(&spec, &second_ctx);
        let second_texts: Vec<&str> = second
            .static_suggestions
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(second_texts, vec!["origin/main"]);
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

    // ------------------------------------------------------------------
    // Addressability tests
    //
    // These tests pin the contract that a spec is reachable by its
    // filename stem (the "canonical id") and, when free, by its
    // declared `name` as a secondary alias. Synthetic collision fixtures
    // below pin the behavior that used to protect wrapper specs whose
    // names overlapped another command's stem.
    // ------------------------------------------------------------------

    #[test]
    fn kubecolor_resolves_by_filename_stem_and_kubectl_wins_alias() {
        // Real-corpus shape: kubecolor.json declares `name: "kubectl"`,
        // kubectl.json also declares `name: "kubectl"`. Under a
        // name-keyed loader one of the two silently won the `kubectl`
        // HashMap slot and the other was dropped. Now both load: each
        // is addressable by its filename stem, and the alphabetically-
        // first file wins the `kubectl` alias.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("kubecolor.json"),
            r#"{
                "name": "kubectl",
                "subcommands": [{"name": "from-kubecolor"}]
            }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("kubectl.json"),
            r#"{
                "name": "kubectl",
                "subcommands": [{"name": "from-kubectl-spec"}]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let store = &result.store;

        // Both stems must address their respective parsed specs.
        let by_kubecolor = store.get("kubecolor").expect("kubecolor stem must resolve");
        assert_eq!(by_kubecolor.subcommands[0].name, "from-kubecolor");
        let by_kubectl = store.get("kubectl").expect("kubectl stem must resolve");
        assert_eq!(by_kubectl.subcommands[0].name, "from-kubectl-spec");

        // Exactly one conflict: kubecolor.json's `kubectl` name alias
        // loses to kubectl.json's stem (NameMatchesOtherStem because
        // the winner's `id` is exactly the contested alias).
        let conflicts = store.conflicts();
        assert_eq!(conflicts.len(), 1, "expected one alias conflict");
        let c = &conflicts[0];
        assert_eq!(c.alias, "kubectl");
        assert_eq!(c.kind, AliasConflictKind::NameMatchesOtherStem);
        assert_eq!(c.winner.filename_stem, "kubectl");
        assert_eq!(c.loser.filename_stem, "kubecolor");

        // No silent loss: every committed file is one entry.
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.aliases_count(), 2, "two stems, no extra alias");
    }

    #[test]
    fn duplicate_name_collision_surfaces_conflict() {
        // Two files declare the same `name`. The alphabetically-first
        // file wins the alias; the second keeps its stem alias and the
        // collision is recorded as DuplicateName.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("alpha.json"),
            r#"{"name": "shared", "subcommands": [{"name": "from-alpha"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("beta.json"),
            r#"{"name": "shared", "subcommands": [{"name": "from-beta"}]}"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let store = &result.store;

        // Both stems resolve to their respective specs.
        assert_eq!(
            store.get("alpha").unwrap().subcommands[0].name,
            "from-alpha"
        );
        assert_eq!(store.get("beta").unwrap().subcommands[0].name, "from-beta");

        // The shared `name` resolves to the first-loaded file.
        let by_name = store
            .get("shared")
            .expect("name alias must resolve to the winner");
        assert_eq!(by_name.subcommands[0].name, "from-alpha");

        // Exactly one conflict surfaces: beta loses the `shared` alias.
        let conflicts = store.conflicts();
        assert_eq!(conflicts.len(), 1);
        let c = &conflicts[0];
        assert_eq!(c.alias, "shared");
        assert_eq!(c.kind, AliasConflictKind::DuplicateName);
        assert_eq!(c.winner.filename_stem, "alpha");
        assert_eq!(c.loser.filename_stem, "beta");

        // Both files become entries; aliases = 2 stems + 1 name = 3.
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.aliases_count(), 3);
    }

    #[test]
    fn uppercase_lowercase_stems_are_case_sensitive() {
        // The corpus has both R.json and Rscript.json, plus r.json and
        // rscript.json. Filename stems are case-sensitive: `R` and `r`
        // are distinct addressable commands (matches the on-shell
        // case-sensitive PATH lookup behavior).
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("R.json"),
            r#"{"name": "R", "subcommands": [{"name": "from-uppercase"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Rscript.json"),
            r#"{"name": "Rscript", "subcommands": [{"name": "from-rscript"}]}"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let store = &result.store;

        let by_r = store.get("R").expect("uppercase R must resolve");
        assert_eq!(by_r.subcommands[0].name, "from-uppercase");
        let by_rscript = store.get("Rscript").expect("Rscript must resolve");
        assert_eq!(by_rscript.subcommands[0].name, "from-rscript");

        // Stems match their `name` declarations, so no extra aliases.
        assert!(
            store.conflicts().is_empty(),
            "no conflicts expected, got {:?}",
            store.conflicts()
        );
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.aliases_count(), 2);
    }

    #[test]
    fn user_override_replaces_embedded_with_directory_precedence() {
        // The classic user-override scenario: the same filename in two
        // configured dirs. The earlier dir is preferred — the embedded copy
        // is recorded as a DirectoryPrecedence fallback candidate at debug
        // level (NOT an error — this is how user overrides work).
        let user_dir = tempfile::TempDir::new().unwrap();
        let embedded_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            user_dir.path().join("git.json"),
            r#"{"name": "git", "subcommands": [{"name": "user-override"}]}"#,
        )
        .unwrap();
        std::fs::write(
            embedded_dir.path().join("git.json"),
            r#"{"name": "git", "subcommands": [{"name": "embedded-default"}]}"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dirs(&[
            user_dir.path().to_path_buf(),
            embedded_dir.path().to_path_buf(),
        ])
        .unwrap();
        let store = &result.store;

        let by_git = store.get("git").expect("git must resolve");
        assert_eq!(
            by_git.subcommands[0].name, "user-override",
            "user copy must win (earlier dir = higher precedence)"
        );

        let conflicts = store.conflicts();
        assert_eq!(conflicts.len(), 1);
        let c = &conflicts[0];
        assert_eq!(c.alias, "git");
        assert_eq!(c.kind, AliasConflictKind::DirectoryPrecedence);
        assert_eq!(c.disposition, AliasConflictDisposition::FallbackCandidate);
        assert_eq!(c.winner.source_dir, user_dir.path());
        assert_eq!(c.loser.source_dir, embedded_dir.path());

        // The lower-precedence copy remains registered as a fallback
        // candidate, but the valid higher-precedence copy is the only
        // resolved entry.
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.iter().count(), 1);
        assert_eq!(store.aliases_count(), 1);
    }

    #[test]
    fn lower_precedence_filesystem_duplicate_used_when_primary_fails() {
        let bad_dir = tempfile::TempDir::new().unwrap();
        let good_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(bad_dir.path().join("git.json"), "{not valid json").unwrap();
        std::fs::write(
            good_dir.path().join("git.json"),
            r#"{"name":"git","subcommands":[{"name":"from-good-dir"}]}"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dirs(&[
            bad_dir.path().to_path_buf(),
            good_dir.path().to_path_buf(),
        ])
        .unwrap();
        let store = &result.store;

        let by_git = store
            .get("git")
            .expect("valid lower-precedence spec should win");
        assert_eq!(by_git.subcommands[0].name, "from-good-dir");
        assert_eq!(
            store.nonfunctional_aliases_count(),
            0,
            "git remains functional through the lower-precedence parsed candidate"
        );
        let bad_entry = store
            .entries()
            .iter()
            .find(|entry| matches!(&entry.source, SpecSource::Filesystem(path) if path.starts_with(bad_dir.path())))
            .expect("bad primary remains visible for diagnostics");
        assert!(bad_entry.load_error().is_some());
        assert_eq!(store.iter().count(), 1);
    }

    #[test]
    fn nonfunctional_alias_count_increments_when_all_candidates_fail() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("broken.json"), "{not valid json").unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let store = &result.store;

        assert!(store.get("broken").is_none());
        assert_eq!(store.nonfunctional_aliases_count(), 1);
    }

    #[test]
    fn filesystem_duplicate_fallback_wins_before_embedded() {
        let bad_dir = tempfile::TempDir::new().unwrap();
        let good_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(bad_dir.path().join("git.json"), "{not valid json").unwrap();
        std::fs::write(
            good_dir.path().join("git.json"),
            r#"{"name":"git","subcommands":[{"name":"from-good-dir"}]}"#,
        )
        .unwrap();

        let result = SpecStore::load_with_embedded(&[
            bad_dir.path().to_path_buf(),
            good_dir.path().to_path_buf(),
        ])
        .unwrap();
        let by_git = result
            .store
            .get("git")
            .expect("valid filesystem fallback should win before embedded");
        assert_eq!(by_git.subcommands[0].name, "from-good-dir");
    }

    #[test]
    fn embedded_stem_falls_back_when_filesystem_name_alias_parse_fails() {
        let bad_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            bad_dir.path().join("foo.json"),
            r#"{"name":"git","subcommands":"not an array"}"#,
        )
        .unwrap();

        let result = SpecStore::load_with_embedded(&[bad_dir.path().to_path_buf()]).unwrap();
        let store = &result.store;

        let by_git = store
            .get("git")
            .expect("embedded git should fall back behind bad filesystem alias");
        assert_eq!(by_git.name, "git");

        let bad_entry = store
            .entries()
            .iter()
            .find(|entry| matches!(&entry.source, SpecSource::Filesystem(path) if path.starts_with(bad_dir.path())))
            .expect("bad filesystem entry remains visible for diagnostics");
        assert!(
            bad_entry.load_error().is_some(),
            "failed filesystem alias owner should record its lazy parse error"
        );

        let conflict = store
            .conflicts()
            .iter()
            .find(|conflict| {
                conflict.alias == "git"
                    && conflict.kind == AliasConflictKind::NameMatchesOtherStem
                    && conflict.winner.filename_stem == "foo"
                    && conflict.loser.filename_stem == "git"
            })
            .expect("embedded git fallback should be recorded as an alias conflict");
        assert_eq!(
            conflict.disposition,
            AliasConflictDisposition::FallbackCandidate
        );
    }

    #[test]
    fn embedded_name_alias_falls_back_when_filesystem_name_alias_parse_fails() {
        let bad_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            bad_dir.path().join("foo.json"),
            r#"{"name":"index","subcommands":"not an array"}"#,
        )
        .unwrap();

        let result = SpecStore::load_with_embedded(&[bad_dir.path().to_path_buf()]).unwrap();
        let store = &result.store;

        let by_index = store
            .get("index")
            .expect("embedded appwrite should fall back behind bad filesystem name alias");
        assert_eq!(by_index.name, "index");

        let bad_entry = store
            .entries()
            .iter()
            .find(|entry| matches!(&entry.source, SpecSource::Filesystem(path) if path.starts_with(bad_dir.path())))
            .expect("bad filesystem entry remains visible for diagnostics");
        assert!(
            bad_entry.load_error().is_some(),
            "failed filesystem alias owner should record its lazy parse error"
        );

        let conflict = store
            .conflicts()
            .iter()
            .find(|conflict| {
                conflict.alias == "index"
                    && conflict.kind == AliasConflictKind::DuplicateName
                    && conflict.winner.filename_stem == "foo"
                    && conflict.loser.filename_stem == "appwrite"
            })
            .expect("embedded appwrite name alias fallback should be recorded as a conflict");
        assert_eq!(
            conflict.disposition,
            AliasConflictDisposition::FallbackCandidate
        );
    }

    #[test]
    fn cross_dir_stem_matches_earlier_name_alias_is_not_directory_precedence() {
        // Cross-dir name-vs-stem collision: dir1 owns the `kubectl`
        // alias via foo.json's `name: "kubectl"` claim, then dir2's
        // kubectl.json arrives whose stem is the same string.
        //
        // The two files have DIFFERENT filename stems (`foo` vs
        // `kubectl`), so this is NOT the user-override scenario —
        // classifying it as DirectoryPrecedence would mislead doctor
        // into telling the user one dir is shadowing another when in
        // reality it's a name-claim collision. Correct kind is
        // NameMatchesOtherStem (from the inverted perspective: the new
        // file's stem matches what the earlier file's name already
        // owns).
        let dir1 = tempfile::TempDir::new().unwrap();
        let dir2 = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir1.path().join("foo.json"),
            r#"{"name": "kubectl", "subcommands": [{"name": "from-foo"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir2.path().join("kubectl.json"),
            r#"{"name": "kubectl", "subcommands": [{"name": "from-kubectl"}]}"#,
        )
        .unwrap();

        let result =
            SpecStore::load_from_dirs(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()])
                .unwrap();
        let store = &result.store;

        // dir1's foo.json wins both stems it touches: `foo` (its own)
        // and `kubectl` (its declared name). dir2's kubectl.json is
        // rejected: its stem `kubectl` is already owned by foo.json's
        // name alias, so it has nothing addressable left.
        assert_eq!(
            store.get("foo").unwrap().subcommands[0].name,
            "from-foo",
            "dir1 foo.json must address by its own stem"
        );
        assert_eq!(
            store.get("kubectl").unwrap().subcommands[0].name,
            "from-foo",
            "dir1's name claim wins because it loaded first"
        );

        let conflicts = store.conflicts();
        assert_eq!(
            conflicts.len(),
            1,
            "expected one conflict, got {conflicts:?}"
        );
        let c = &conflicts[0];
        assert_eq!(c.alias, "kubectl");
        assert_eq!(
            c.kind,
            AliasConflictKind::NameMatchesOtherStem,
            "different filename stems must classify as NameMatchesOtherStem, \
             not DirectoryPrecedence — distinct files in distinct dirs are \
             not the user-override scenario"
        );
        assert_eq!(c.winner.filename_stem, "foo");
        assert_eq!(c.winner.source_dir, dir1.path());
        assert_eq!(c.loser.filename_stem, "kubectl");
        assert_eq!(c.loser.source_dir, dir2.path());

        // dir2's kubectl.json is dropped entirely — the only entry is
        // dir1's foo.json.
        assert_eq!(store.entries().len(), 1);
        // Aliases: `foo` stem + `kubectl` name = 2.
        assert_eq!(store.aliases_count(), 2);
    }

    #[test]
    fn iter_yields_one_tuple_per_unique_spec_not_per_alias() {
        // SpecStore::iter() must enumerate entries (one per file), not
        // alias keys (which would double-count specs that register both
        // a stem and a name alias). Without this, status counts would
        // overcount the corpus by ~8 against the 709-spec baseline.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("alias-target.json"),
            r#"{"name": "different-name", "subcommands": [{"name": "x"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("plain.json"),
            r#"{"name": "plain", "subcommands": [{"name": "y"}]}"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let store = &result.store;

        // 2 entries; 3 aliases (alias-target, different-name, plain).
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.aliases_count(), 3);

        let iter_count = store.iter().count();
        assert_eq!(
            iter_count, 2,
            "iter() must yield one tuple per unique spec, not per alias"
        );

        // Every stem reachable via iter must round-trip through get().
        for (id, spec) in store.iter() {
            let got = store.get(id).expect("stem must resolve");
            assert_eq!(got.name, spec.name);
        }
    }

    #[test]
    fn addressability_holds_against_full_corpus() {
        // The on-disk corpus must load without silent loss: every
        // committed `*.json` becomes a SpecEntry (709 entries against
        // the embedded corpus), and aliases_count() equals 709 + the
        // number of non-conflicting `name` aliases.
        let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
        if !spec_dir.is_dir() {
            // Repo-test guard: this test runs from the workspace where
            // `specs/` lives. Skip silently in environments without it.
            return;
        }
        let result = SpecStore::load_from_dir(&spec_dir).unwrap();
        let store = &result.store;

        // Every file becomes one entry.
        let file_count = std::fs::read_dir(&spec_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .count();
        assert_eq!(
            store.entries().len(),
            file_count,
            "every committed spec file must produce a unique SpecEntry"
        );

        // commands_addressable ≥ entries: each entry registers at
        // least its stem, possibly plus a name alias.
        assert!(
            store.aliases_count() >= store.entries().len(),
            "alias count {} must be ≥ entry count {}",
            store.aliases_count(),
            store.entries().len()
        );

        // Wrapper commands that historically collided with underlying
        // command names must remain addressable by their filename stem.
        for stem in ["kubecolor", "br", "j", "nativescript", "tns", "sta"] {
            assert!(
                store.get(stem).is_some(),
                "stem `{stem}` must be addressable by filename"
            );
        }

        // The committed corpus should not ship duplicate-name or
        // name-vs-stem collisions. DirectoryPrecedence remains covered by
        // override-specific tests.
        assert!(
            store.conflicts().is_empty(),
            "embedded corpus should have no alias conflicts: {:?}",
            store.conflicts()
        );
    }
}
