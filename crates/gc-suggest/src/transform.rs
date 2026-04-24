use std::fmt;

use regex::Regex;
use serde::de::{self, MapAccess, Visitor};
use serde::Deserialize;

use crate::json_path::JsonPath;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// A single transform step in a pipeline that processes raw generator output
/// into completion suggestions.
#[derive(Debug, Clone)]
pub enum Transform {
    Named(NamedTransform),
    Parameterized(ParameterizedTransform),
}

/// Simple named transforms that take no configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedTransform {
    SplitLines,
    FilterEmpty,
    Trim,
    SkipFirst,
    Dedup,
}

/// Transforms that require configuration parameters.
#[derive(Debug, Clone)]
pub enum ParameterizedTransform {
    SplitOn {
        delimiter: String,
    },
    Skip {
        n: usize,
    },
    Take {
        n: usize,
    },
    ErrorGuard {
        starts_with: Option<String>,
        contains: Option<String>,
    },
    /// Pre-compiled regex extraction.
    ///
    /// The `Regex` is compiled once at spec-load time (in the `TryFrom`
    /// `ParameterizedHelper` impl below) so a hot generator running on every
    /// keystroke does not pay the regex-compilation cost on each invocation.
    RegexExtract {
        compiled: Regex,
        name: usize,
        description: Option<usize>,
    },
    JsonExtract {
        name: JsonPath,
        description: Option<JsonPath>,
    },
    /// Terminal transform: parse the ENTIRE raw output as JSON, walk `path`
    /// to an array, then emit one suggestion per array element.
    ///
    /// Each element is treated as a string unless `item_name` is supplied,
    /// in which case it's looked up inside the element. An optional
    /// `split_on` + `split_index` applies a final per-element split/pick
    /// (covers the scarb `n.split(" ")[0]` pattern from `@withfig` specs).
    ///
    /// INVARIANT: if `split_index` is `Some`, `split_on` must be `Some`;
    /// and `split_on` must be non-empty if `Some`. Enforced at construction
    /// via `TryFrom<ParameterizedHelper>` and backstopped by `assert!` at
    /// apply time.
    JsonExtractArray {
        path: JsonPath,
        item_name: Option<JsonPath>,
        item_description: Option<JsonPath>,
        split_on: Option<String>,
        split_index: Option<usize>,
    },
    ColumnExtract {
        column: usize,
        description_column: Option<usize>,
    },
    /// Append a fixed literal to each suggestion's text (post-split/post-extract).
    Suffix {
        value: String,
    },
}

/// Returns the display name of a transform for use in error messages.
pub fn transform_name(t: &Transform) -> &'static str {
    match t {
        Transform::Named(n) => match n {
            NamedTransform::SplitLines => "split_lines",
            NamedTransform::FilterEmpty => "filter_empty",
            NamedTransform::Trim => "trim",
            NamedTransform::SkipFirst => "skip_first",
            NamedTransform::Dedup => "dedup",
        },
        Transform::Parameterized(p) => match p {
            ParameterizedTransform::SplitOn { .. } => "split_on",
            ParameterizedTransform::Skip { .. } => "skip",
            ParameterizedTransform::Take { .. } => "take",
            ParameterizedTransform::ErrorGuard { .. } => "error_guard",
            ParameterizedTransform::RegexExtract { .. } => "regex_extract",
            ParameterizedTransform::JsonExtract { .. } => "json_extract",
            ParameterizedTransform::JsonExtractArray { .. } => "json_extract_array",
            ParameterizedTransform::ColumnExtract { .. } => "column_extract",
            ParameterizedTransform::Suffix { .. } => "suffix",
        },
    }
}

const VALID_NAMED_TRANSFORMS: &[&str] =
    &["split_lines", "filter_empty", "trim", "skip_first", "dedup"];

fn parse_named_transform(s: &str) -> Option<NamedTransform> {
    match s {
        "split_lines" => Some(NamedTransform::SplitLines),
        "filter_empty" => Some(NamedTransform::FilterEmpty),
        "trim" => Some(NamedTransform::Trim),
        "skip_first" => Some(NamedTransform::SkipFirst),
        "dedup" => Some(NamedTransform::Dedup),
        _ => None,
    }
}

/// Helper enum for internally-tagged deserialization of parameterized transforms.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum ParameterizedHelper {
    #[serde(rename = "split_on")]
    SplitOn { delimiter: String },
    #[serde(rename = "skip")]
    Skip { n: usize },
    #[serde(rename = "take")]
    Take { n: usize },
    #[serde(rename = "error_guard")]
    ErrorGuard {
        starts_with: Option<String>,
        contains: Option<String>,
    },
    #[serde(rename = "regex_extract")]
    RegexExtract {
        pattern: String,
        name: usize,
        description: Option<usize>,
    },
    #[serde(rename = "json_extract")]
    JsonExtract {
        name: JsonPath,
        description: Option<JsonPath>,
    },
    #[serde(rename = "json_extract_array")]
    JsonExtractArray {
        path: JsonPath,
        item_name: Option<JsonPath>,
        item_description: Option<JsonPath>,
        split_on: Option<String>,
        split_index: Option<usize>,
    },
    #[serde(rename = "column_extract")]
    ColumnExtract {
        column: usize,
        description_column: Option<usize>,
    },
    #[serde(rename = "suffix")]
    Suffix { value: String },
}

impl TryFrom<ParameterizedHelper> for ParameterizedTransform {
    type Error = String;

    fn try_from(h: ParameterizedHelper) -> Result<Self, Self::Error> {
        Ok(match h {
            ParameterizedHelper::SplitOn { delimiter } => {
                // `str::split("")` has surprising Unicode-boundary semantics
                // (emits a token between every char cluster) and will never
                // do what a spec author intended. Reject at load time, matching
                // the sibling check inside `json_extract_array` below.
                if delimiter.is_empty() {
                    return Err("split_on: delimiter must not be empty".to_string());
                }
                ParameterizedTransform::SplitOn { delimiter }
            }
            ParameterizedHelper::Skip { n } => ParameterizedTransform::Skip { n },
            ParameterizedHelper::Take { n } => ParameterizedTransform::Take { n },
            ParameterizedHelper::ErrorGuard {
                starts_with,
                contains,
            } => ParameterizedTransform::ErrorGuard {
                starts_with,
                contains,
            },
            ParameterizedHelper::RegexExtract {
                pattern,
                name,
                description,
            } => {
                // Compile the regex once at spec-load time. Surface a clear,
                // actionable error message that includes the pattern and the
                // crate-provided regex error so a broken spec is easy to fix.
                let compiled = Regex::new(&pattern).map_err(|e| {
                    format!("invalid regex in regex_extract pattern {pattern:?}: {e}")
                })?;
                ParameterizedTransform::RegexExtract {
                    compiled,
                    name,
                    description,
                }
            }
            ParameterizedHelper::JsonExtract { name, description } => {
                ParameterizedTransform::JsonExtract { name, description }
            }
            ParameterizedHelper::JsonExtractArray {
                path,
                item_name,
                item_description,
                split_on,
                split_index,
            } => {
                if split_index.is_some() && split_on.is_none() {
                    return Err(
                        "json_extract_array: split_index requires split_on to be set".to_string(),
                    );
                }
                // `str::split("")` has surprising Unicode-boundary semantics
                // (emits a token between every char cluster) and will never
                // do what a spec author intended. Reject at load time.
                if matches!(split_on.as_deref(), Some("")) {
                    return Err("json_extract_array: split_on must not be empty".to_string());
                }
                ParameterizedTransform::JsonExtractArray {
                    path,
                    item_name,
                    item_description,
                    split_on,
                    split_index,
                }
            }
            ParameterizedHelper::ColumnExtract {
                column,
                description_column,
            } => ParameterizedTransform::ColumnExtract {
                column,
                description_column,
            },
            ParameterizedHelper::Suffix { value } => ParameterizedTransform::Suffix { value },
        })
    }
}

impl<'de> Deserialize<'de> for Transform {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(TransformVisitor)
    }
}

struct TransformVisitor;

impl<'de> Visitor<'de> for TransformVisitor {
    type Value = Transform;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "a named transform string (one of: {}) or a parameterized transform object",
            VALID_NAMED_TRANSFORMS.join(", ")
        )
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match parse_named_transform(v) {
            Some(named) => Ok(Transform::Named(named)),
            None => Err(de::Error::custom(format!(
                "unknown transform \"{v}\"; valid named transforms are: {}",
                VALID_NAMED_TRANSFORMS.join(", ")
            ))),
        }
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let helper: ParameterizedHelper =
            Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;
        let parameterized = ParameterizedTransform::try_from(helper).map_err(de::Error::custom)?;
        Ok(Transform::Parameterized(parameterized))
    }
}

/// Validate that a transform pipeline follows ordering rules.
///
/// Rules:
/// - `error_guard` must appear before any split transform
/// - `split_lines` / `split_on` must appear at most once
/// - Post-split transforms (`filter_empty`, `trim`, `skip_first`, `dedup`,
///   `regex_extract`, `json_extract`, `column_extract`, `skip`, `take`)
///   must not appear before the split
/// - Empty pipeline is valid
pub fn validate_pipeline(transforms: &[Transform]) -> Result<(), String> {
    if transforms.is_empty() {
        return Ok(());
    }

    let mut seen_split = false;
    let mut seen_json_array = false;
    let mut split_count = 0;
    let mut json_array_count = 0;

    for (i, t) in transforms.iter().enumerate() {
        let name = transform_name(t);

        // Once json_extract_array has fired it has set `suggestions = Some(_)`
        // and any subsequent line-mutating transform is silently discarded
        // (or, in the case of a second json_extract, silently overwrites the
        // first's results). The only legitimately valid post-extract
        // transform is `suffix`, whose executor arm operates on the
        // `suggestions` slot. Reject everything else at load time.
        //
        // A second `json_extract_array` is left to fall through to the
        // dedicated duplicate check below (which produces a more specific
        // error message and is already covered by
        // `test_multiple_json_extract_array_invalid`).
        let is_suffix = matches!(
            t,
            Transform::Parameterized(ParameterizedTransform::Suffix { .. })
        );
        let is_json_array_candidate = matches!(
            t,
            Transform::Parameterized(ParameterizedTransform::JsonExtractArray { .. })
        );
        if seen_json_array && !is_suffix && !is_json_array_candidate {
            return Err(format!(
                "transform \"{name}\" at position {i} appears after json_extract_array; \
                 json_extract_array is terminal and only suffix is allowed after it"
            ));
        }

        // json_extract_array is a standalone terminal transform that operates
        // on the raw output — it must not be combined with split_lines/split_on
        // (those would pre-split the JSON into unparseable fragments) and
        // must appear at most once (a second entry would silently overwrite
        // the first's suggestions at runtime).
        let is_json_array = matches!(
            t,
            Transform::Parameterized(ParameterizedTransform::JsonExtractArray { .. })
        );
        if is_json_array {
            if seen_split {
                return Err(format!(
                    "json_extract_array at position {i} appears after a split transform; \
                     json_extract_array consumes the raw output directly and cannot follow split_lines/split_on"
                ));
            }
            json_array_count += 1;
            if json_array_count > 1 {
                return Err(
                    "transform pipeline has multiple json_extract_array transforms; \
                     only one is allowed"
                        .to_string(),
                );
            }
            seen_json_array = true;
            continue;
        }

        // Check split transforms
        let is_split = matches!(
            t,
            Transform::Named(NamedTransform::SplitLines)
                | Transform::Parameterized(ParameterizedTransform::SplitOn { .. })
        );

        if is_split {
            split_count += 1;
            if split_count > 1 {
                return Err("transform pipeline has multiple split transforms; \
                     only one of split_lines/split_on is allowed"
                    .to_string());
            }
            seen_split = true;
            continue;
        }

        // Check error_guard must be before split
        let is_error_guard = matches!(
            t,
            Transform::Parameterized(ParameterizedTransform::ErrorGuard { .. })
        );
        if is_error_guard {
            if seen_split {
                return Err(format!(
                    "error_guard at position {i} appears after a split transform; \
                     error_guard must appear before split_lines/split_on"
                ));
            }
            continue;
        }

        // All other transforms are post-split: they must not appear before the split
        let is_post_split = matches!(
            t,
            Transform::Named(NamedTransform::FilterEmpty)
                | Transform::Named(NamedTransform::Trim)
                | Transform::Named(NamedTransform::SkipFirst)
                | Transform::Named(NamedTransform::Dedup)
                | Transform::Parameterized(ParameterizedTransform::RegexExtract { .. })
                | Transform::Parameterized(ParameterizedTransform::JsonExtract { .. })
                | Transform::Parameterized(ParameterizedTransform::ColumnExtract { .. })
                | Transform::Parameterized(ParameterizedTransform::Skip { .. })
                | Transform::Parameterized(ParameterizedTransform::Take { .. })
                | Transform::Parameterized(ParameterizedTransform::Suffix { .. })
        );

        if is_post_split && !seen_split && !seen_json_array {
            return Err(format!(
                "transform \"{name}\" at position {i} appears before any split transform; \
                 post-split transforms must appear after split_lines/split_on"
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Named transform implementations
// ---------------------------------------------------------------------------

/// Split raw output on newline boundaries.
pub fn apply_split_lines(output: &str) -> Vec<String> {
    output.split('\n').map(String::from).collect()
}

/// Split raw output on a custom delimiter.
pub fn apply_split_on(output: &str, delimiter: &str) -> Vec<String> {
    output.split(delimiter).map(String::from).collect()
}

/// Remove empty and whitespace-only lines.
pub fn apply_filter_empty(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// Trim leading/trailing whitespace from each line.
pub fn apply_trim(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| line.trim().to_string())
        .collect()
}

/// Drop the first element (header line).
pub fn apply_skip_first(lines: Vec<String>) -> Vec<String> {
    if lines.is_empty() {
        return lines;
    }
    lines.into_iter().skip(1).collect()
}

/// Drop the first N elements.
pub fn apply_skip(lines: Vec<String>, n: usize) -> Vec<String> {
    lines.into_iter().skip(n).collect()
}

/// Keep only the first N elements.
pub fn apply_take(lines: Vec<String>, n: usize) -> Vec<String> {
    lines.into_iter().take(n).collect()
}

/// Remove consecutive duplicates (like Unix `uniq`).
pub fn apply_dedup(lines: Vec<String>) -> Vec<String> {
    let mut result = Vec::with_capacity(lines.len());
    for line in lines {
        if result.last().is_none_or(|prev: &String| *prev != line) {
            result.push(line);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Parameterized transform implementations
// ---------------------------------------------------------------------------

/// Guard against error output. Returns `None` to abort pipeline, `Some(output)` to continue.
pub fn apply_error_guard(
    output: &str,
    starts_with: Option<&str>,
    contains: Option<&str>,
) -> Option<String> {
    if let Some(prefix) = starts_with {
        if output.starts_with(prefix) {
            return None;
        }
    }
    if let Some(needle) = contains {
        if output.contains(needle) {
            return None;
        }
    }
    Some(output.to_string())
}

/// Extract fields from each line using a pre-compiled regex with capture groups.
/// Lines that don't match are silently skipped.
///
/// Takes the compiled `Regex` by reference — the regex is compiled once at
/// spec-load time and reused across every invocation.
pub fn apply_regex_extract(
    lines: &[String],
    re: &Regex,
    name_group: usize,
    desc_group: Option<usize>,
) -> Vec<Suggestion> {
    lines
        .iter()
        .filter_map(|line| {
            let caps = re.captures(line)?;
            let text = caps.get(name_group)?.as_str().to_string();
            let description = desc_group.and_then(|g| caps.get(g).map(|m| m.as_str().to_string()));
            Some(Suggestion {
                text,
                description,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Script,
                ..Default::default()
            })
        })
        .collect()
}

/// Parse each line as JSON and extract fields by dotted path.
///
/// A flat single-segment path (e.g. `"Name"`) is equivalent to a top-level
/// `obj.get("Name")` and preserves back-compat with the prior flat-field
/// behaviour. Nested paths like `foo.bar.baz` walk into the object.
pub fn apply_json_extract(
    lines: &[String],
    name_path: &JsonPath,
    desc_path: Option<&JsonPath>,
) -> Vec<Suggestion> {
    lines
        .iter()
        .filter_map(|line| {
            let obj: serde_json::Value = serde_json::from_str(line).ok()?;
            let text = name_path.lookup(&obj)?.as_str()?.to_string();
            let description =
                desc_path.and_then(|dp| dp.lookup(&obj).and_then(|v| v.as_str()).map(String::from));
            Some(Suggestion {
                text,
                description,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Script,
                ..Default::default()
            })
        })
        .collect()
}

/// Parse the ENTIRE raw output as a single JSON blob, walk `path` to an array,
/// and emit one suggestion per element. Used for Fig patterns like
/// `JSON.parse(out).foo.bar.map(e => ({name: e}))` where the data hangs off a
/// dotted path and is NOT newline-delimited.
///
/// Element-to-text rules:
/// - If `item_name` is `Some`, look it up on each element (object shape).
/// - Else, treat the element itself as the value:
///     * string elements → use as-is
///     * primitive elements (number/bool) → stringified
/// - If `split_on` is set, apply `split(split_on)[split_index]` to the text
///   (default index = 0). Mirrors the `.split(" ")[0]` pattern common in
///   `@withfig/autocomplete` generators.
pub fn apply_json_extract_array(
    raw_output: &str,
    path: &JsonPath,
    item_name: Option<&JsonPath>,
    item_description: Option<&JsonPath>,
    split_on: Option<&str>,
    split_index: Option<usize>,
) -> Vec<Suggestion> {
    let root: serde_json::Value = match serde_json::from_str(raw_output) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("json_extract_array: failed to parse subprocess output as JSON: {e}");
            return Vec::new();
        }
    };
    let Some(arr) = path.lookup(&root).and_then(|v| v.as_array()) else {
        tracing::debug!(
            ?path,
            "json_extract_array: path did not resolve to a JSON array in parsed output"
        );
        return Vec::new();
    };
    let idx = split_index.unwrap_or(0);

    arr.iter()
        .filter_map(|el| {
            let raw_text = match item_name {
                Some(p) => p.lookup(el).and_then(value_to_text)?,
                None => value_to_text(el)?,
            };
            let text = match split_on {
                Some(sep) => raw_text.split(sep).nth(idx)?.to_string(),
                None => raw_text,
            };
            let description = item_description
                .and_then(|p| p.lookup(el).and_then(|v| v.as_str()).map(String::from));
            Some(Suggestion {
                text,
                description,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Script,
                ..Default::default()
            })
        })
        .collect()
}

fn value_to_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Append a fixed literal to each suggestion's text.
pub fn apply_suffix(suggestions: &mut [Suggestion], value: &str) {
    for s in suggestions.iter_mut() {
        s.text.push_str(value);
    }
}

/// Split each line by whitespace and extract columns by index.
pub fn apply_column_extract(
    lines: &[String],
    column: usize,
    description_column: Option<usize>,
) -> Vec<Suggestion> {
    lines
        .iter()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let text = parts.get(column)?.to_string();
            let description =
                description_column.and_then(|dc| parts.get(dc).map(|s| s.to_string()));
            Some(Suggestion {
                text,
                description,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Script,
                ..Default::default()
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Pipeline executor
// ---------------------------------------------------------------------------

/// Execute a transform pipeline against raw command output.
/// Two-phase: pre-split (error_guard), then split, then post-split.
pub fn execute_pipeline(output: &str, transforms: &[Transform]) -> Result<Vec<Suggestion>, String> {
    let mut current_output = output.to_string();
    let mut lines: Option<Vec<String>> = None;
    let mut suggestions: Option<Vec<Suggestion>> = None;

    // Materialize `lines` from `current_output` once on first post-split use;
    // subsequent stages mutate it in place to avoid per-stage Vec reallocation.
    fn ensure_lines<'a>(
        lines: &'a mut Option<Vec<String>>,
        current_output: &mut String,
    ) -> &'a mut Vec<String> {
        if lines.is_none() {
            *lines = Some(vec![std::mem::take(current_output)]);
        }
        // Invariant: the branch above guarantees `Some`. Use `.expect` with a
        // descriptive message so a future refactor that breaks the invariant
        // produces an actionable panic instead of a bare unwrap.
        lines.as_mut().expect("lines just initialized above")
    }

    for transform in transforms {
        match transform {
            // Pre-split: error_guard
            Transform::Parameterized(ParameterizedTransform::ErrorGuard {
                starts_with,
                contains,
            }) => {
                match apply_error_guard(
                    &current_output,
                    starts_with.as_deref(),
                    contains.as_deref(),
                ) {
                    None => return Ok(Vec::new()),
                    Some(out) => current_output = out,
                }
            }

            // Split transforms
            Transform::Named(NamedTransform::SplitLines) => {
                lines = Some(apply_split_lines(&current_output));
            }
            Transform::Parameterized(ParameterizedTransform::SplitOn { delimiter }) => {
                lines = Some(apply_split_on(&current_output, delimiter));
            }

            // Post-split named transforms — mutate in place
            Transform::Named(NamedTransform::FilterEmpty) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                l.retain(|line| !line.trim().is_empty());
            }
            Transform::Named(NamedTransform::Trim) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                for line in l.iter_mut() {
                    let end_ws = line.len() - line.trim_end().len();
                    if end_ws > 0 {
                        line.truncate(line.len() - end_ws);
                    }
                    let start_ws = line.len() - line.trim_start().len();
                    if start_ws > 0 {
                        line.drain(..start_ws);
                    }
                }
            }
            Transform::Named(NamedTransform::SkipFirst) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                if !l.is_empty() {
                    l.drain(..1);
                }
            }
            Transform::Named(NamedTransform::Dedup) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                l.dedup();
            }

            // Post-split parameterized transforms — mutate in place
            Transform::Parameterized(ParameterizedTransform::Skip { n }) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                let take_n = (*n).min(l.len());
                l.drain(..take_n);
            }
            Transform::Parameterized(ParameterizedTransform::Take { n }) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                l.truncate(*n);
            }

            // Extract transforms: produce Vec<Suggestion>
            Transform::Parameterized(ParameterizedTransform::RegexExtract {
                compiled,
                name,
                description,
            }) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                suggestions = Some(apply_regex_extract(l, compiled, *name, *description));
            }
            Transform::Parameterized(ParameterizedTransform::JsonExtract { name, description }) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                suggestions = Some(apply_json_extract(l, name, description.as_ref()));
            }
            Transform::Parameterized(ParameterizedTransform::JsonExtractArray {
                path,
                item_name,
                item_description,
                split_on,
                split_index,
            }) => {
                // Terminal transform on the raw output. `validate_pipeline`
                // rejects any prior split, so `current_output` still holds
                // the full blob here.
                //
                // Cross-field invariants on `JsonExtractArray` are enforced
                // at deserialize time by `TryFrom<ParameterizedHelper>`, but
                // the struct's fields are `pub` and direct construction
                // bypasses that check. Panic loudly on misuse in both debug
                // and release so a future caller cannot silently produce
                // garbage completions.
                assert!(
                    !(split_index.is_some() && split_on.is_none()),
                    "JsonExtractArray: split_index requires split_on",
                );
                assert!(
                    split_on.as_deref() != Some(""),
                    "JsonExtractArray: split_on must not be empty",
                );
                suggestions = Some(apply_json_extract_array(
                    &current_output,
                    path,
                    item_name.as_ref(),
                    item_description.as_ref(),
                    split_on.as_deref(),
                    *split_index,
                ));
            }
            Transform::Parameterized(ParameterizedTransform::ColumnExtract {
                column,
                description_column,
            }) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                suggestions = Some(apply_column_extract(l, *column, *description_column));
            }
            Transform::Parameterized(ParameterizedTransform::Suffix { value }) => {
                if let Some(s) = suggestions.as_mut() {
                    apply_suffix(s, value);
                } else {
                    let l = ensure_lines(&mut lines, &mut current_output);
                    for line in l.iter_mut() {
                        line.push_str(value);
                    }
                }
            }
        }
    }

    // If an extract already produced suggestions, return them.
    if let Some(s) = suggestions {
        return Ok(s);
    }

    // Otherwise, convert remaining lines to plain suggestions.
    let final_lines = lines.unwrap_or_else(|| vec![current_output]);
    Ok(final_lines
        .into_iter()
        .map(|text| Suggestion {
            text,
            kind: SuggestionKind::Command,
            source: SuggestionSource::Script,
            ..Default::default()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Named transform deserialization --

    #[test]
    fn test_deserialize_split_lines() {
        let t: Transform = serde_json::from_str(r#""split_lines""#).unwrap();
        assert!(matches!(t, Transform::Named(NamedTransform::SplitLines)));
    }

    #[test]
    fn test_deserialize_filter_empty() {
        let t: Transform = serde_json::from_str(r#""filter_empty""#).unwrap();
        assert!(matches!(t, Transform::Named(NamedTransform::FilterEmpty)));
    }

    #[test]
    fn test_deserialize_trim() {
        let t: Transform = serde_json::from_str(r#""trim""#).unwrap();
        assert!(matches!(t, Transform::Named(NamedTransform::Trim)));
    }

    #[test]
    fn test_deserialize_skip_first() {
        let t: Transform = serde_json::from_str(r#""skip_first""#).unwrap();
        assert!(matches!(t, Transform::Named(NamedTransform::SkipFirst)));
    }

    #[test]
    fn test_deserialize_dedup() {
        let t: Transform = serde_json::from_str(r#""dedup""#).unwrap();
        assert!(matches!(t, Transform::Named(NamedTransform::Dedup)));
    }

    // -- Parameterized transform deserialization --

    #[test]
    fn test_deserialize_split_on() {
        let t: Transform =
            serde_json::from_str(r#"{"type": "split_on", "delimiter": ","}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::SplitOn { delimiter }) => {
                assert_eq!(delimiter, ",");
            }
            other => panic!("expected SplitOn, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_skip() {
        let t: Transform = serde_json::from_str(r#"{"type": "skip", "n": 3}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::Skip { n }) => {
                assert_eq!(n, 3);
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_take() {
        let t: Transform = serde_json::from_str(r#"{"type": "take", "n": 10}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::Take { n }) => {
                assert_eq!(n, 10);
            }
            other => panic!("expected Take, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_error_guard() {
        let t: Transform = serde_json::from_str(
            r#"{"type": "error_guard", "starts_with": "error:", "contains": "fatal"}"#,
        )
        .unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::ErrorGuard {
                starts_with,
                contains,
            }) => {
                assert_eq!(starts_with.as_deref(), Some("error:"));
                assert_eq!(contains.as_deref(), Some("fatal"));
            }
            other => panic!("expected ErrorGuard, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_error_guard_partial() {
        let t: Transform =
            serde_json::from_str(r#"{"type": "error_guard", "starts_with": "ERR"}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::ErrorGuard {
                starts_with,
                contains,
            }) => {
                assert_eq!(starts_with.as_deref(), Some("ERR"));
                assert!(contains.is_none());
            }
            other => panic!("expected ErrorGuard, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_regex_extract() {
        let t: Transform = serde_json::from_str(
            r#"{"type": "regex_extract", "pattern": "^(\\S+)\\s+(.*)", "name": 1, "description": 2}"#,
        )
        .unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::RegexExtract {
                compiled,
                name,
                description,
            }) => {
                // The compiled pattern should round-trip via `as_str()` so a
                // valid spec is observably preserved.
                assert_eq!(compiled.as_str(), r"^(\S+)\s+(.*)");
                assert_eq!(name, 1);
                assert_eq!(description, Some(2));
            }
            other => panic!("expected RegexExtract, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_regex_extract_invalid_pattern_fails() {
        // Garbage pattern: unbalanced bracket. Deserialization MUST fail with
        // a clear error message that mentions both the pattern and the regex
        // crate's diagnostic — otherwise a typo in a spec is undebuggable.
        let bad = r#"{"type": "regex_extract", "pattern": "[unclosed", "name": 1}"#;
        let err = serde_json::from_str::<Transform>(bad).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("regex_extract"),
            "error must mention transform name: {msg}"
        );
        assert!(
            msg.contains("[unclosed"),
            "error must echo the broken pattern: {msg}"
        );
    }

    #[test]
    fn test_deserialize_json_extract() {
        let t: Transform = serde_json::from_str(
            r#"{"type": "json_extract", "name": "name", "description": "desc"}"#,
        )
        .unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::JsonExtract { name, description }) => {
                assert!(name.is_flat());
                assert!(description.is_some());
            }
            other => panic!("expected JsonExtract, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_json_extract_dotted_path() {
        // Nested dotted paths must round-trip through the Deserialize impl
        // so spec authors can describe data structures like {"foo": {"bar": "x"}}.
        let t: Transform =
            serde_json::from_str(r#"{"type": "json_extract", "name": "foo.bar"}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::JsonExtract { name, .. }) => {
                assert!(!name.is_flat());
            }
            other => panic!("expected JsonExtract, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_json_extract_invalid_path_fails() {
        // A malformed path (trailing dot) must surface as a spec-load error,
        // not as silent runtime no-op behaviour.
        let err = serde_json::from_str::<Transform>(r#"{"type": "json_extract", "name": "foo."}"#)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("trailing"),
            "error should mention trailing dot: {msg}"
        );
    }

    #[test]
    fn test_deserialize_json_extract_array() {
        let t: Transform =
            serde_json::from_str(r#"{"type": "json_extract_array", "path": "project.schemes"}"#)
                .unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::JsonExtractArray {
                path,
                item_name,
                split_on,
                split_index,
                ..
            }) => {
                assert!(!path.is_flat());
                assert!(item_name.is_none());
                assert!(split_on.is_none());
                assert!(split_index.is_none());
            }
            other => panic!("expected JsonExtractArray, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_json_extract_array_with_item_description() {
        // Pin the Deserialize round-trip for `item_description`: a refactor
        // that accidentally drops the field from the helper enum would
        // silently lose every spec-author's description config, and the
        // existing `..` destructures elsewhere would not catch it.
        let t: Transform = serde_json::from_str(
            r#"{"type": "json_extract_array", "path": "items", "item_description": "label"}"#,
        )
        .unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::JsonExtractArray {
                item_description,
                ..
            }) => {
                assert!(
                    item_description.is_some(),
                    "item_description must round-trip through Deserialize"
                );
            }
            other => panic!("expected JsonExtractArray, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_json_extract_array_with_split() {
        let t: Transform = serde_json::from_str(
            r#"{"type": "json_extract_array", "path": "workspace.members", "split_on": " ", "split_index": 0}"#,
        )
        .unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::JsonExtractArray {
                split_on,
                split_index,
                ..
            }) => {
                assert_eq!(split_on.as_deref(), Some(" "));
                assert_eq!(split_index, Some(0));
            }
            other => panic!("expected JsonExtractArray, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_json_extract_array_split_index_without_split_on_fails() {
        // Config error: split_index is meaningless without split_on. Reject
        // at spec-load rather than silently ignoring.
        let err = serde_json::from_str::<Transform>(
            r#"{"type": "json_extract_array", "path": "a.b", "split_index": 2}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("split_on"));
    }

    #[test]
    fn test_split_on_empty_delimiter_rejected() {
        // `str::split("")` has surprising Unicode-boundary semantics that
        // never match spec-author intent — mirror the sibling check on
        // `json_extract_array.split_on` and reject at load time.
        let err = serde_json::from_str::<Transform>(r#"{"type": "split_on", "delimiter": ""}"#)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("split_on") && msg.contains("must not be empty"),
            "error should name split_on and explain empty delimiter: {msg}"
        );
    }

    #[test]
    fn test_json_extract_array_empty_split_on_rejected() {
        // `str::split("")` has surprising Unicode-boundary semantics. A spec
        // author writing `"split_on": ""` is ~always wrong — reject at load
        // time with an actionable message rather than producing weird
        // completions at runtime.
        let err = serde_json::from_str::<Transform>(
            r#"{"type": "json_extract_array", "path": "a.b", "split_on": ""}"#,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("split_on must not be empty"),
            "error should mention the empty split_on: {msg}"
        );
    }

    #[test]
    fn test_deserialize_column_extract() {
        let t: Transform = serde_json::from_str(
            r#"{"type": "column_extract", "column": 0, "description_column": 2}"#,
        )
        .unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::ColumnExtract {
                column,
                description_column,
            }) => {
                assert_eq!(column, 0);
                assert_eq!(description_column, Some(2));
            }
            other => panic!("expected ColumnExtract, got {other:?}"),
        }
    }

    // -- Error cases --

    #[test]
    fn test_unknown_named_transform_error() {
        let err = serde_json::from_str::<Transform>(r#""not_a_transform""#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown transform"),
            "error should mention unknown transform: {msg}"
        );
        assert!(
            msg.contains("split_lines"),
            "error should list valid transforms: {msg}"
        );
    }

    // -- Array deserialization --

    #[test]
    fn test_deserialize_transform_array() {
        let transforms: Vec<Transform> = serde_json::from_str(
            r#"["split_lines", "filter_empty", {"type": "regex_extract", "pattern": "^(\\S+)", "name": 1}]"#,
        )
        .unwrap();
        assert_eq!(transforms.len(), 3);
        assert!(matches!(
            transforms[0],
            Transform::Named(NamedTransform::SplitLines)
        ));
        assert!(matches!(
            transforms[1],
            Transform::Named(NamedTransform::FilterEmpty)
        ));
        assert!(matches!(
            transforms[2],
            Transform::Parameterized(ParameterizedTransform::RegexExtract { .. })
        ));
    }

    // -- Pipeline validation --

    #[test]
    fn test_valid_pipeline() {
        let pipeline = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Named(NamedTransform::FilterEmpty),
            Transform::Named(NamedTransform::Trim),
        ];
        assert!(validate_pipeline(&pipeline).is_ok());
    }

    #[test]
    fn test_error_guard_before_split_valid() {
        let pipeline = vec![
            Transform::Parameterized(ParameterizedTransform::ErrorGuard {
                starts_with: Some("error".into()),
                contains: None,
            }),
            Transform::Named(NamedTransform::SplitLines),
            Transform::Named(NamedTransform::FilterEmpty),
        ];
        assert!(validate_pipeline(&pipeline).is_ok());
    }

    #[test]
    fn test_error_guard_after_split_invalid() {
        let pipeline = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Parameterized(ParameterizedTransform::ErrorGuard {
                starts_with: None,
                contains: Some("fatal".into()),
            }),
        ];
        let err = validate_pipeline(&pipeline).unwrap_err();
        assert!(
            err.contains("error_guard"),
            "error should mention error_guard: {err}"
        );
    }

    #[test]
    fn test_post_split_before_split_invalid() {
        let pipeline = vec![
            Transform::Named(NamedTransform::FilterEmpty),
            Transform::Named(NamedTransform::SplitLines),
        ];
        let err = validate_pipeline(&pipeline).unwrap_err();
        assert!(
            err.contains("filter_empty"),
            "error should mention filter_empty: {err}"
        );
        assert!(
            err.contains("before any split"),
            "error should mention ordering: {err}"
        );
    }

    #[test]
    fn test_double_split_invalid() {
        let pipeline = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Parameterized(ParameterizedTransform::SplitOn {
                delimiter: ",".into(),
            }),
        ];
        let err = validate_pipeline(&pipeline).unwrap_err();
        assert!(
            err.contains("multiple split"),
            "error should mention multiple splits: {err}"
        );
    }

    #[test]
    fn test_empty_pipeline_valid() {
        assert!(validate_pipeline(&[]).is_ok());
    }

    #[test]
    fn test_transform_name_coverage() {
        // Ensure transform_name returns the expected strings
        assert_eq!(
            transform_name(&Transform::Named(NamedTransform::SplitLines)),
            "split_lines"
        );
        assert_eq!(
            transform_name(&Transform::Named(NamedTransform::FilterEmpty)),
            "filter_empty"
        );
        assert_eq!(
            transform_name(&Transform::Named(NamedTransform::Trim)),
            "trim"
        );
        assert_eq!(
            transform_name(&Transform::Named(NamedTransform::SkipFirst)),
            "skip_first"
        );
        assert_eq!(
            transform_name(&Transform::Named(NamedTransform::Dedup)),
            "dedup"
        );
        assert_eq!(
            transform_name(&Transform::Parameterized(ParameterizedTransform::SplitOn {
                delimiter: ",".into()
            })),
            "split_on"
        );
        assert_eq!(
            transform_name(&Transform::Parameterized(
                ParameterizedTransform::ErrorGuard {
                    starts_with: None,
                    contains: None
                }
            )),
            "error_guard"
        );
    }

    // -- Named transform execution --

    #[test]
    fn test_split_lines() {
        let result = apply_split_lines("foo\nbar\nbaz\n");
        assert_eq!(result, vec!["foo", "bar", "baz", ""]);
    }

    #[test]
    fn test_split_on() {
        let result = apply_split_on("a,b,c", ",");
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_filter_empty() {
        let input = vec!["foo".into(), "".into(), "  ".into(), "bar".into()];
        let result = apply_filter_empty(input);
        assert_eq!(result, vec!["foo", "bar"]);
    }

    #[test]
    fn test_trim() {
        let input = vec!["  foo  ".into(), "bar  ".into()];
        let result = apply_trim(input);
        assert_eq!(result, vec!["foo", "bar"]);
    }

    #[test]
    fn test_skip_first() {
        let input = vec!["header".into(), "data1".into(), "data2".into()];
        let result = apply_skip_first(input);
        assert_eq!(result, vec!["data1", "data2"]);
    }

    #[test]
    fn test_skip() {
        let input = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let result = apply_skip(input, 2);
        assert_eq!(result, vec!["c", "d"]);
    }

    #[test]
    fn test_take() {
        let input = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let result = apply_take(input, 2);
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn test_dedup() {
        let input = vec!["a".into(), "a".into(), "b".into(), "b".into(), "a".into()];
        let result = apply_dedup(input);
        assert_eq!(result, vec!["a", "b", "a"]);
    }

    // -- Parameterized transform execution --

    #[test]
    fn test_error_guard_blocks() {
        let result = apply_error_guard("Error: not found", Some("Error:"), None);
        assert!(result.is_none());
    }

    #[test]
    fn test_error_guard_passes() {
        let result = apply_error_guard("foo bar", Some("Error:"), None);
        assert!(result.is_some());
    }

    #[test]
    fn test_error_guard_contains() {
        let result = apply_error_guard("something error occurred", None, Some("error"));
        assert!(result.is_none());
    }

    #[test]
    fn test_error_guard_both_match_returns_none() {
        // Both `starts_with` and `contains` set, both match the input —
        // either is sufficient to block. Pin the "both match → block"
        // path alongside the single-predicate tests above.
        let result = apply_error_guard("Err: boom", Some("Err"), Some("boom"));
        assert!(result.is_none());
    }

    #[test]
    fn test_error_guard_only_contains_matches_returns_none() {
        // `starts_with` set but does NOT match; `contains` set and DOES
        // match. The contains arm alone must be enough to block — this
        // was previously untested.
        let result = apply_error_guard("it will fail", Some("ZZZ"), Some("fail"));
        assert!(result.is_none());
    }

    #[test]
    fn test_error_guard_both_none_passes_through() {
        // Both predicates unset: the guard is a pure pass-through. Pin
        // this explicitly so a refactor that inverts the predicate
        // polarity cannot silently turn no-op guards into hard blocks.
        let result = apply_error_guard("anything", None, None);
        assert_eq!(result.as_deref(), Some("anything"));
    }

    #[test]
    fn test_regex_extract() {
        let lines = vec!["nginx   running".into(), "redis   stopped".into()];
        let re = Regex::new(r"^(\S+)\s+(\S+)").unwrap();
        let result = apply_regex_extract(&lines, &re, 1, Some(2));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "nginx");
        assert_eq!(result[0].description.as_deref(), Some("running"));
        // Pin kind/source so removing the explicit Command/Script assignment
        // (and thereby falling back to `Suggestion::default()`'s ProviderValue)
        // is caught.
        assert_eq!(result[0].kind, SuggestionKind::Command);
        assert_eq!(result[0].source, SuggestionSource::Script);
    }

    #[test]
    fn test_regex_extract_no_match_skipped() {
        let lines = vec!["matches_pattern".into(), "".into(), "also_matches".into()];
        let re = Regex::new(r"^(\S+)$").unwrap();
        let result = apply_regex_extract(&lines, &re, 1, None);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_regex_extract_compiled_once_reused_many_times() {
        // The whole point of LOW-3: a single compiled regex must be reusable
        // across many calls without recompiling. Compile once, run a thousand
        // extractions, verify each call still produces the right result.
        let re = Regex::new(r"^item_(\d+)$").unwrap();
        for i in 0..1000 {
            let line = format!("item_{i}");
            let result = apply_regex_extract(std::slice::from_ref(&line), &re, 1, None);
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].text, i.to_string());
        }
    }

    #[test]
    fn test_json_extract() {
        let lines = vec![
            r#"{"Name":"nginx","Status":"running"}"#.into(),
            r#"{"Name":"redis","Status":"stopped"}"#.into(),
        ];
        let name = JsonPath::parse("Name").unwrap();
        let desc = JsonPath::parse("Status").unwrap();
        let result = apply_json_extract(&lines, &name, Some(&desc));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "nginx");
        assert_eq!(result[0].description.as_deref(), Some("running"));
        // Guard against silent regressions where removing the explicit
        // Command/Script assignment lets `Suggestion::default()` take over.
        assert_eq!(result[0].kind, SuggestionKind::Command);
        assert_eq!(result[0].source, SuggestionSource::Script);
    }

    #[test]
    fn test_json_extract_with_dollar_prefix() {
        let lines = vec![r#"{"Name":"test"}"#.into()];
        let name = JsonPath::parse("$.Name").unwrap();
        let result = apply_json_extract(&lines, &name, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "test");
    }

    #[test]
    fn test_json_extract_non_string_description_becomes_none() {
        // `as_str()` on a non-string JSON value returns None. The line
        // must still emit a suggestion — the text field is a string so
        // the row survives — but the description must be None rather
        // than a stringified number. Pins the silent-drop-on-non-string
        // desc behaviour.
        let lines = vec![r#"{"name":"x","count":42}"#.into()];
        let name = JsonPath::parse("name").unwrap();
        let desc = JsonPath::parse("count").unwrap();
        let result = apply_json_extract(&lines, &name, Some(&desc));
        assert_eq!(result.len(), 1, "row must emit, not filter");
        assert_eq!(result[0].text, "x");
        assert!(
            result[0].description.is_none(),
            "non-string desc value must become None, got: {:?}",
            result[0].description
        );
    }

    #[test]
    fn test_json_extract_nested_path() {
        // Dotted path should walk into nested objects on a per-line basis.
        let lines = vec![
            r#"{"meta":{"name":"nginx","status":"running"}}"#.into(),
            r#"{"meta":{"name":"redis","status":"stopped"}}"#.into(),
        ];
        let name = JsonPath::parse("meta.name").unwrap();
        let desc = JsonPath::parse("meta.status").unwrap();
        let result = apply_json_extract(&lines, &name, Some(&desc));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "nginx");
        assert_eq!(result[0].description.as_deref(), Some("running"));
    }

    #[test]
    fn test_json_extract_array_strings() {
        // parse-map-6 shape: dotted path lands on an array of strings
        // and each element becomes a suggestion text.
        let raw = r#"{"project":{"schemes":["Debug","Release","Staging"]}}"#;
        let path = JsonPath::parse("project.schemes").unwrap();
        let result = apply_json_extract_array(raw, &path, None, None, None, None);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "Debug");
        assert_eq!(result[2].text, "Staging");
        // Pin kind/source — same silent-regression guard as the other
        // extract-path tests.
        assert_eq!(result[0].kind, SuggestionKind::Command);
        assert_eq!(result[0].source, SuggestionSource::Script);
    }

    #[test]
    fn test_json_extract_array_with_item_name() {
        // If elements are objects, item_name picks the field to emit.
        let raw = r#"{"items":[{"id":"a","label":"Alpha"},{"id":"b","label":"Beta"}]}"#;
        let path = JsonPath::parse("items").unwrap();
        let item_name = JsonPath::parse("label").unwrap();
        let result = apply_json_extract_array(raw, &path, Some(&item_name), None, None, None);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "Alpha");
        assert_eq!(result[1].text, "Beta");
    }

    #[test]
    fn test_json_extract_array_with_item_description() {
        // `item_description` must thread into the emitted Suggestion's
        // `description`. A refactor that dropped the executor pass-through
        // would produce completions with text but no description — this
        // asserts per-element description ends up where spec authors
        // expect it.
        let raw = r#"{"items":[{"id":"a","label":"Alpha"},{"id":"b","label":"Beta"}]}"#;
        let path = JsonPath::parse("items").unwrap();
        let item_name = JsonPath::parse("id").unwrap();
        let item_description = JsonPath::parse("label").unwrap();
        let result = apply_json_extract_array(
            raw,
            &path,
            Some(&item_name),
            Some(&item_description),
            None,
            None,
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "a");
        assert_eq!(result[0].description.as_deref(), Some("Alpha"));
        assert_eq!(result[1].text, "b");
        assert_eq!(result[1].description.as_deref(), Some("Beta"));
    }

    #[test]
    fn test_json_extract_array_with_split() {
        // parse-map-split shape: string element needs `.split(" ")[0]` to
        // trim off the version suffix.
        let raw = r#"{"workspace":{"members":["pkg_a 0.1.0","pkg_b 2.3.4"]}}"#;
        let path = JsonPath::parse("workspace.members").unwrap();
        let result = apply_json_extract_array(raw, &path, None, None, Some(" "), Some(0));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "pkg_a");
        assert_eq!(result[1].text, "pkg_b");
    }

    #[test]
    fn test_json_extract_array_missing_path_returns_empty() {
        let raw = r#"{"other":{"members":[]}}"#;
        let path = JsonPath::parse("workspace.members").unwrap();
        let result = apply_json_extract_array(raw, &path, None, None, None, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_json_extract_array_invalid_json_returns_empty() {
        let path = JsonPath::parse("foo").unwrap();
        let result = apply_json_extract_array("not json at all", &path, None, None, None, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_column_extract() {
        let lines = vec![
            "abc123  some description".into(),
            "def456  other desc".into(),
        ];
        let result = apply_column_extract(&lines, 0, Some(1));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "abc123");
        assert!(result[0].description.is_some());
        // Pin kind/source to catch silent Command/Script -> default()
        // regressions.
        assert_eq!(result[0].kind, SuggestionKind::Command);
        assert_eq!(result[0].source, SuggestionSource::Script);
    }

    #[test]
    fn test_column_extract_short_line_filtered() {
        // A line with fewer whitespace-separated parts than `column`
        // must be silently dropped (the `parts.get(column)?` short-circuits
        // the filter_map closure). Pin this drop-on-underflow behaviour so
        // a future refactor that promoted the `?` to `unwrap_or` would
        // fail the test instead of producing empty-text suggestions.
        let lines = vec!["abc".into(), "def ghi".into()];
        let result = apply_column_extract(&lines, 1, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "ghi");
    }

    #[test]
    fn test_column_extract_missing_description_column_keeps_row() {
        // A line that has enough parts for `column` but not for
        // `description_column` must still emit the row, with
        // `description: None`. Pins the split asymmetry between the
        // required text column (drops row) and the optional description
        // column (emits row with None).
        let lines = vec!["a b".into()];
        let result = apply_column_extract(&lines, 0, Some(5));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "a");
        assert!(result[0].description.is_none());
    }

    // -- Pipeline executor --

    #[test]
    fn test_execute_pipeline_basic() {
        let transforms: Vec<Transform> =
            serde_json::from_str(r#"["split_lines", "filter_empty", "trim"]"#).unwrap();
        let output = "  foo  \n\n  bar  \n  baz  \n";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "foo");
        assert_eq!(result[1].text, "bar");
        assert_eq!(result[2].text, "baz");
    }

    #[test]
    fn test_execute_pipeline_with_error_guard_blocks() {
        let transforms: Vec<Transform> = serde_json::from_str(
            r#"[{"type": "error_guard", "starts_with": "Error:"}, "split_lines", "filter_empty"]"#,
        )
        .unwrap();
        let output = "Error: command not found";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_execute_pipeline_with_error_guard_passes() {
        let transforms: Vec<Transform> = serde_json::from_str(
            r#"[{"type": "error_guard", "starts_with": "Error:"}, "split_lines", "filter_empty"]"#,
        )
        .unwrap();
        let output = "foo\nbar\n";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_execute_pipeline_regex_extract() {
        let transforms: Vec<Transform> = serde_json::from_str(
            r#"["split_lines", "skip_first", "filter_empty", {"type": "regex_extract", "pattern": "^(\\S+)\\s+(\\S+)", "name": 1, "description": 2}]"#,
        )
        .unwrap();
        let output = "NAME    STATUS\nnginx   running\nredis   stopped\n";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "nginx");
        assert_eq!(result[0].description.as_deref(), Some("running"));
    }

    #[test]
    fn test_execute_pipeline_json_extract() {
        let transforms: Vec<Transform> = serde_json::from_str(
            r#"["split_lines", "filter_empty", {"type": "json_extract", "name": "Name", "description": "Status"}]"#,
        )
        .unwrap();
        let output =
            "{\"Name\":\"nginx\",\"Status\":\"running\"}\n{\"Name\":\"redis\",\"Status\":\"stopped\"}";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "nginx");
    }

    #[test]
    fn test_execute_pipeline_json_extract_array() {
        let transforms: Vec<Transform> =
            serde_json::from_str(r#"[{"type": "json_extract_array", "path": "project.schemes"}]"#)
                .unwrap();
        let output = r#"{"project":{"schemes":["Debug","Release"]}}"#;
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "Debug");
        assert_eq!(result[1].text, "Release");
    }

    #[test]
    fn test_execute_pipeline_json_extract_array_with_split() {
        let transforms: Vec<Transform> = serde_json::from_str(
            r#"[{"type": "json_extract_array", "path": "workspace.members", "split_on": " ", "split_index": 0}]"#,
        )
        .unwrap();
        let output = r#"{"workspace":{"members":["alpha 1.0.0","beta 2.0.0"]}}"#;
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "alpha");
        assert_eq!(result[1].text, "beta");
    }

    #[test]
    fn test_validate_pipeline_rejects_json_extract_array_after_split() {
        let pipeline: Vec<Transform> = serde_json::from_str(
            r#"["split_lines", {"type": "json_extract_array", "path": "a.b"}]"#,
        )
        .unwrap();
        let err = validate_pipeline(&pipeline).unwrap_err();
        assert!(
            err.contains("json_extract_array"),
            "error must mention the offending transform: {err}"
        );
    }

    #[test]
    fn test_validate_pipeline_rejects_split_lines_after_json_extract_array() {
        // json_extract_array is terminal — any line-mutating transform that
        // follows it runs against the (now-stale) `lines` slot and its effect
        // is silently discarded when the executor returns `suggestions`.
        let pipeline: Vec<Transform> = serde_json::from_str(
            r#"[{"type": "json_extract_array", "path": "a.b"}, "split_lines"]"#,
        )
        .unwrap();
        let err = validate_pipeline(&pipeline).unwrap_err();
        assert!(
            err.contains("split_lines") && err.contains("json_extract_array"),
            "error must name the offending transform and the terminal transform: {err}"
        );
    }

    #[test]
    fn test_validate_pipeline_rejects_filter_empty_after_json_extract_array() {
        let pipeline: Vec<Transform> = serde_json::from_str(
            r#"[{"type": "json_extract_array", "path": "a.b"}, "filter_empty"]"#,
        )
        .unwrap();
        let err = validate_pipeline(&pipeline).unwrap_err();
        assert!(
            err.contains("filter_empty") && err.contains("json_extract_array"),
            "error must name the offending transform and the terminal transform: {err}"
        );
    }

    #[test]
    fn test_validate_pipeline_rejects_json_extract_after_json_extract_array() {
        // A follow-up `json_extract` would silently overwrite the suggestions
        // produced by `json_extract_array` at runtime — reject at load time.
        let pipeline: Vec<Transform> = serde_json::from_str(
            r#"[{"type": "json_extract_array", "path": "a.b"}, {"type": "json_extract", "name": "x"}]"#,
        )
        .unwrap();
        let err = validate_pipeline(&pipeline).unwrap_err();
        assert!(
            err.contains("json_extract") && err.contains("json_extract_array"),
            "error must name the offending transform and the terminal transform: {err}"
        );
    }

    #[test]
    fn test_validate_pipeline_suffix_after_json_extract_array_ok() {
        // `suffix` is the one exception — its executor arm explicitly mutates
        // the `suggestions` slot when present, so it composes legitimately.
        let pipeline: Vec<Transform> = serde_json::from_str(
            r#"[{"type": "json_extract_array", "path": "a.b"}, {"type": "suffix", "value": "="}]"#,
        )
        .unwrap();
        assert!(validate_pipeline(&pipeline).is_ok());
    }

    #[test]
    fn test_multiple_json_extract_array_invalid() {
        // Two json_extract_array transforms in one pipeline would silently
        // overwrite the first's suggestions at runtime. Reject at load time.
        let pipeline: Vec<Transform> = serde_json::from_str(
            r#"[{"type": "json_extract_array", "path": "a.b"}, {"type": "json_extract_array", "path": "c.d"}]"#,
        )
        .unwrap();
        let err = validate_pipeline(&pipeline).unwrap_err();
        assert!(
            err.contains("multiple json_extract_array"),
            "error should mention multiple json_extract_array: {err}"
        );
    }

    // -- Suffix transform --

    #[test]
    fn test_deserialize_suffix() {
        let t: Transform = serde_json::from_str(r#"{"type":"suffix","value":"="}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::Suffix { value }) => {
                assert_eq!(value, "=");
            }
            other => panic!("expected Suffix, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_suffix_basic() {
        let mut suggestions = vec![
            Suggestion {
                text: "foo".into(),
                ..Default::default()
            },
            Suggestion {
                text: "bar".into(),
                ..Default::default()
            },
        ];
        apply_suffix(&mut suggestions, "=");
        assert_eq!(suggestions[0].text, "foo=");
        assert_eq!(suggestions[1].text, "bar=");
    }

    #[test]
    fn test_execute_pipeline_suffix_after_json_extract() {
        let transforms: Vec<Transform> = serde_json::from_str(
            r#"["split_lines","filter_empty",{"type":"json_extract","name":"Name","description":"Image"},{"type":"suffix","value":"="}]"#,
        )
        .unwrap();
        let output =
            "{\"Name\":\"web\",\"Image\":\"nginx\"}\n{\"Name\":\"db\",\"Image\":\"redis\"}";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "web=");
        assert_eq!(result[0].description.as_deref(), Some("nginx"));
        assert_eq!(result[1].text, "db=");
    }

    #[test]
    fn test_execute_pipeline_suffix_on_plain_lines() {
        let transforms: Vec<Transform> =
            serde_json::from_str(r#"["split_lines","filter_empty",{"type":"suffix","value":"!"}]"#)
                .unwrap();
        let output = "alpha\nbeta\n";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "alpha!");
        assert_eq!(result[1].text, "beta!");
    }

    #[test]
    fn test_validate_pipeline_suffix_after_split_ok() {
        let pipeline: Vec<Transform> =
            serde_json::from_str(r#"["split_lines","filter_empty",{"type":"suffix","value":"="}]"#)
                .unwrap();
        assert!(validate_pipeline(&pipeline).is_ok());
    }

    #[test]
    fn test_execute_pipeline_no_split_treats_as_single_item() {
        let transforms: Vec<Transform> = vec![];
        let output = "single_item";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "single_item");
        // This is the fallthrough branch at the bottom of `execute_pipeline`
        // (the "Otherwise, convert remaining lines to plain suggestions"
        // block). It's the only path for a transforms-present-but-no-extract
        // pipeline and MUST stamp kind=Command/source=Script rather than
        // defaulting to ProviderValue.
        assert_eq!(result[0].kind, SuggestionKind::Command);
        assert_eq!(result[0].source, SuggestionSource::Script);
    }
}
