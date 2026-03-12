use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::Deserialize;

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
    RegexExtract {
        pattern: String,
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

impl From<ParameterizedHelper> for ParameterizedTransform {
    fn from(h: ParameterizedHelper) -> Self {
        match h {
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
            } => ParameterizedTransform::RegexExtract {
                pattern,
                name,
                description,
            },
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
        }
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
        Ok(Transform::Parameterized(helper.into()))
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
                return Err(
                    "transform pipeline has multiple split transforms; \
                     only one of split_lines/split_on is allowed"
                        .to_string(),
                );
            }
            seen_split = true;
            continue;
        }

        // Check error_guard must be before split
        let is_error_guard =
            matches!(t, Transform::Parameterized(ParameterizedTransform::ErrorGuard { .. }));
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
                pattern,
                name,
                description,
            }) => {
                assert_eq!(pattern, r"^(\S+)\s+(.*)");
                assert_eq!(name, 1);
                assert_eq!(description, Some(2));
            }
            other => panic!("expected RegexExtract, got {other:?}"),
        }
    }

    #[test]
    fn test_deserialize_json_extract() {
        let t: Transform = serde_json::from_str(
            r#"{"type": "json_extract", "name": "name", "description": "desc"}"#,
        )
        .unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::JsonExtract {
                name,
                description,
            }) => {
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
            transform_name(&Transform::Parameterized(ParameterizedTransform::ErrorGuard {
                starts_with: None,
                contains: None
            })),
            "error_guard"
        );
    }
}
