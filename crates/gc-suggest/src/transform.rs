use std::fmt;

use regex::Regex;
use serde::de::{self, MapAccess, Visitor};
use serde::Deserialize;

use crate::types::{Suggestion, SuggestionSource};

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
        name: String,
        description: Option<String>,
    },
    ColumnExtract {
        column: usize,
        description_column: Option<usize>,
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
            ParameterizedTransform::ColumnExtract { .. } => "column_extract",
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
        name: String,
        description: Option<String>,
    },
    #[serde(rename = "column_extract")]
    ColumnExtract {
        column: usize,
        description_column: Option<usize>,
    },
}

impl TryFrom<ParameterizedHelper> for ParameterizedTransform {
    type Error = String;

    fn try_from(h: ParameterizedHelper) -> Result<Self, Self::Error> {
        Ok(match h {
            ParameterizedHelper::SplitOn { delimiter } => {
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
            ParameterizedHelper::ColumnExtract {
                column,
                description_column,
            } => ParameterizedTransform::ColumnExtract {
                column,
                description_column,
            },
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
    let mut split_count = 0;

    for (i, t) in transforms.iter().enumerate() {
        let name = transform_name(t);

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
        );

        if is_post_split && !seen_split {
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
                source: SuggestionSource::Script,
                ..Default::default()
            })
        })
        .collect()
}

/// Parse each line as JSON and extract fields by top-level key name.
/// Strips `$.` prefix from paths if present (simplified JSONPath).
pub fn apply_json_extract(
    lines: &[String],
    name_path: &str,
    desc_path: Option<&str>,
) -> Vec<Suggestion> {
    let name_key = name_path.strip_prefix("$.").unwrap_or(name_path);
    let desc_key = desc_path.map(|p| p.strip_prefix("$.").unwrap_or(p));

    lines
        .iter()
        .filter_map(|line| {
            let obj: serde_json::Value = serde_json::from_str(line).ok()?;
            let text = obj.get(name_key)?.as_str()?.to_string();
            let description =
                desc_key.and_then(|dk| obj.get(dk).and_then(|v| v.as_str()).map(String::from));
            Some(Suggestion {
                text,
                description,
                source: SuggestionSource::Script,
                ..Default::default()
            })
        })
        .collect()
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
        lines.as_mut().unwrap()
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
                suggestions = Some(apply_json_extract(l, name, description.as_deref()));
            }
            Transform::Parameterized(ParameterizedTransform::ColumnExtract {
                column,
                description_column,
            }) => {
                let l = ensure_lines(&mut lines, &mut current_output);
                suggestions = Some(apply_column_extract(l, *column, *description_column));
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
                assert_eq!(name, "name");
                assert_eq!(description.as_deref(), Some("desc"));
            }
            other => panic!("expected JsonExtract, got {other:?}"),
        }
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
    fn test_regex_extract() {
        let lines = vec!["nginx   running".into(), "redis   stopped".into()];
        let re = Regex::new(r"^(\S+)\s+(\S+)").unwrap();
        let result = apply_regex_extract(&lines, &re, 1, Some(2));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "nginx");
        assert_eq!(result[0].description.as_deref(), Some("running"));
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
        let result = apply_json_extract(&lines, "Name", Some("Status"));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "nginx");
        assert_eq!(result[0].description.as_deref(), Some("running"));
    }

    #[test]
    fn test_json_extract_with_dollar_prefix() {
        let lines = vec![r#"{"Name":"test"}"#.into()];
        let result = apply_json_extract(&lines, "$.Name", None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "test");
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
    fn test_execute_pipeline_no_split_treats_as_single_item() {
        let transforms: Vec<Transform> = vec![];
        let output = "single_item";
        let result = execute_pipeline(output, &transforms).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "single_item");
    }
}
