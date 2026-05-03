//! Normalize JS return values into [`crate::JsSuggestion`]s.
//!
//! The strategy: serialise the JS value via QuickJS' built-in
//! `JSON.stringify`, then parse the resulting JSON in Rust. This sidesteps
//! several pitfalls in one move:
//!
//! - Cyclic objects throw on `JSON.stringify`, so we don't need our own
//!   cycle detector.
//! - Functions, symbols, and host objects either omit themselves from
//!   the JSON or render as `null`, which we reject as `InvalidShape`.
//! - Strings produced by JS are UTF-16; the JSON pass gives us proper
//!   UTF-8.
//!
//! The cost is one extra serialise/parse round trip, but corpus output
//! is small (a few hundred suggestions) so this is comfortably under
//! our latency budget.

use crate::types::{JsDiagnostic, JsDiagnosticCode, JsRuntimeOutput, JsSuggestion};

use rquickjs::{CatchResultExt, Ctx, Value};
use serde_json::Value as Json;

/// Maximum number of suggestions a single generator may produce.
///
/// Anything beyond this is truncated and an [`JsDiagnosticCode::OversizedOutput`]
/// diagnostic is appended.
pub const MAX_SUGGESTIONS: usize = 1024;

/// Maximum byte length of a single suggestion `name` (UTF-8).
pub const MAX_NAME_LEN: usize = 256;

/// Maximum byte length of a suggestion `description` (UTF-8).
pub const MAX_DESCRIPTION_LEN: usize = 1024;

/// Maximum byte length of the JSON serialisation of the JS return
/// value. Anything larger short-circuits to
/// [`JsDiagnosticCode::OversizedOutput`] before we allocate
/// `serde_json::Value`s.
pub const MAX_TOTAL_OUTPUT_BYTES: usize = 256 * 1024;

/// Top-level entry point invoked by the worker.
///
/// Caller has already unwrapped any returned Promise; `value` is the
/// final synchronous JS value to normalise.
pub(crate) fn normalize_value<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> JsRuntimeOutput {
    // Treat undefined / null as an empty result with a diagnostic — the
    // caller can decide whether that's a hard failure or a soft empty.
    if value.is_undefined() || value.is_null() {
        return JsRuntimeOutput::empty_with(JsDiagnostic {
            code: JsDiagnosticCode::EmptyOutput,
            message: "JS evaluation produced undefined or null".into(),
        });
    }

    // Stringify via QuickJS so we get a sane JSON dump (and cycles
    // throw cleanly).
    let stringified = match ctx.json_stringify(value).catch(ctx) {
        Ok(Some(s)) => match s.to_string() {
            Ok(s) => s,
            Err(e) => {
                return JsRuntimeOutput::empty_with(JsDiagnostic {
                    code: JsDiagnosticCode::InvalidShape,
                    message: format!("could not decode stringified JS: {e}"),
                });
            }
        },
        Ok(None) => {
            // None means the value serialised to JS `undefined` (e.g. a
            // bare function). Treat as InvalidShape.
            return JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::InvalidShape,
                message: "JS value is not JSON-serialisable (e.g. a function)".into(),
            });
        }
        Err(err) => {
            return JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::InvalidShape,
                message: format!("JSON.stringify failed: {err}"),
            });
        }
    };

    if stringified.len() > MAX_TOTAL_OUTPUT_BYTES {
        return JsRuntimeOutput::empty_with(JsDiagnostic {
            code: JsDiagnosticCode::OversizedOutput,
            message: format!(
                "JSON serialisation produced {} bytes (max {MAX_TOTAL_OUTPUT_BYTES})",
                stringified.len()
            ),
        });
    }

    let json: Json = match serde_json::from_str(&stringified) {
        Ok(j) => j,
        Err(e) => {
            return JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::InvalidShape,
                message: format!("could not parse JSON output: {e}"),
            });
        }
    };

    normalize_json(json)
}

/// Pure Rust normalization once we have a `serde_json::Value`. Split out
/// for unit testing without spinning up QuickJS.
pub(crate) fn normalize_json(json: Json) -> JsRuntimeOutput {
    let mut output = JsRuntimeOutput::default();

    match json {
        Json::Null => {
            output.diagnostics.push(JsDiagnostic {
                code: JsDiagnosticCode::EmptyOutput,
                message: "JS evaluation produced null".into(),
            });
        }
        Json::Bool(_) | Json::Number(_) => {
            output.diagnostics.push(JsDiagnostic {
                code: JsDiagnosticCode::InvalidShape,
                message: format!(
                    "expected string / object / array, got {}",
                    primitive_kind(&json)
                ),
            });
        }
        Json::String(s) => match push_string(&mut output, s) {
            Ok(()) => {}
            Err(d) => output.diagnostics.push(d),
        },
        Json::Array(arr) => {
            let oversized = arr.len() > MAX_SUGGESTIONS;
            let truncated_count = arr.len();
            for (idx, item) in arr.into_iter().enumerate() {
                if output.suggestions.len() >= MAX_SUGGESTIONS {
                    break;
                }
                if let Err(d) = push_array_item(&mut output, idx, item) {
                    output.diagnostics.push(d);
                }
            }
            if oversized {
                output.diagnostics.push(JsDiagnostic {
                    code: JsDiagnosticCode::OversizedOutput,
                    message: format!(
                        "array of {truncated_count} items exceeded MAX_SUGGESTIONS \
                         ({MAX_SUGGESTIONS}); truncated"
                    ),
                });
            }
            if output.suggestions.is_empty()
                && !output
                    .diagnostics
                    .iter()
                    .any(|d| d.code == JsDiagnosticCode::OversizedOutput)
            {
                output.diagnostics.push(JsDiagnostic {
                    code: JsDiagnosticCode::EmptyOutput,
                    message: "JS array produced no suggestions".into(),
                });
            }
        }
        Json::Object(map) => match push_object(&mut output, map) {
            Ok(()) => {}
            Err(d) => output.diagnostics.push(d),
        },
    }

    output
}

fn primitive_kind(j: &Json) -> &'static str {
    match j {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

fn push_string(output: &mut JsRuntimeOutput, s: String) -> Result<(), JsDiagnostic> {
    if s.is_empty() {
        return Err(JsDiagnostic {
            code: JsDiagnosticCode::InvalidShape,
            message: "empty suggestion name".into(),
        });
    }
    if s.len() > MAX_NAME_LEN {
        return Err(JsDiagnostic {
            code: JsDiagnosticCode::OversizedOutput,
            message: format!("suggestion name has {} bytes (max {MAX_NAME_LEN})", s.len()),
        });
    }
    output.suggestions.push(JsSuggestion {
        name: s,
        description: None,
    });
    Ok(())
}

fn push_array_item(
    output: &mut JsRuntimeOutput,
    idx: usize,
    item: Json,
) -> Result<(), JsDiagnostic> {
    match item {
        Json::String(s) => push_string(output, s),
        Json::Object(map) => {
            let mut tmp = JsRuntimeOutput::default();
            push_object(&mut tmp, map)?;
            // push_object emits at most one suggestion; keep diagnostics.
            output.suggestions.extend(tmp.suggestions);
            output.diagnostics.extend(tmp.diagnostics);
            Ok(())
        }
        other => Err(JsDiagnostic {
            code: JsDiagnosticCode::InvalidShape,
            message: format!(
                "array element [{idx}] is {}, expected string or object",
                primitive_kind(&other)
            ),
        }),
    }
}

fn push_object(
    output: &mut JsRuntimeOutput,
    mut map: serde_json::Map<String, Json>,
) -> Result<(), JsDiagnostic> {
    // Fig specs use `name` for the displayed text and `description`
    // for the secondary line. Some JS generators still use Fig's
    // `displayName` or `text` aliases; we accept all three.
    let name_value = map
        .remove("name")
        .or_else(|| map.remove("displayName"))
        .or_else(|| map.remove("text"));

    let raw_name = match name_value {
        Some(Json::String(s)) if !s.is_empty() => s,
        Some(Json::String(_)) => {
            return Err(JsDiagnostic {
                code: JsDiagnosticCode::InvalidShape,
                message: "object suggestion has empty name".into(),
            });
        }
        Some(other) => {
            return Err(JsDiagnostic {
                code: JsDiagnosticCode::InvalidShape,
                message: format!(
                    "object suggestion has non-string name ({})",
                    primitive_kind(&other)
                ),
            });
        }
        None => {
            return Err(JsDiagnostic {
                code: JsDiagnosticCode::InvalidShape,
                message: "object suggestion is missing name/displayName/text".into(),
            });
        }
    };

    if raw_name.len() > MAX_NAME_LEN {
        return Err(JsDiagnostic {
            code: JsDiagnosticCode::OversizedOutput,
            message: format!(
                "suggestion name has {} bytes (max {MAX_NAME_LEN})",
                raw_name.len()
            ),
        });
    }

    let description = match map.remove("description") {
        Some(Json::String(s)) if s.is_empty() => None,
        Some(Json::String(s)) if s.len() > MAX_DESCRIPTION_LEN => {
            return Err(JsDiagnostic {
                code: JsDiagnosticCode::OversizedOutput,
                message: format!(
                    "suggestion description has {} bytes (max {MAX_DESCRIPTION_LEN})",
                    s.len()
                ),
            });
        }
        Some(Json::String(s)) => Some(s),
        Some(Json::Null) | None => None,
        Some(other) => {
            return Err(JsDiagnostic {
                code: JsDiagnosticCode::InvalidShape,
                message: format!(
                    "object suggestion has non-string description ({})",
                    primitive_kind(&other)
                ),
            });
        }
    };

    output.suggestions.push(JsSuggestion {
        name: raw_name,
        description,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_value_becomes_one_suggestion() {
        let out = normalize_json(json!("hello"));
        assert_eq!(out.suggestions.len(), 1);
        assert_eq!(out.suggestions[0].name, "hello");
        assert!(out.suggestions[0].description.is_none());
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn empty_string_is_invalid_shape() {
        let out = normalize_json(json!(""));
        assert!(out.suggestions.is_empty());
        assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::InvalidShape);
    }

    #[test]
    fn string_array_becomes_multiple_suggestions() {
        let out = normalize_json(json!(["a", "b", "c"]));
        assert_eq!(out.suggestions.len(), 3);
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn object_array_uses_name_and_description() {
        let out = normalize_json(json!([
            {"name": "main", "description": "primary branch"},
            {"name": "dev"},
            {"displayName": "feat/x"},
            {"text": "release/1.0"}
        ]));
        assert_eq!(out.suggestions.len(), 4);
        assert_eq!(out.suggestions[0].name, "main");
        assert_eq!(
            out.suggestions[0].description.as_deref(),
            Some("primary branch")
        );
        assert!(out.suggestions[1].description.is_none());
        assert_eq!(out.suggestions[2].name, "feat/x");
        assert_eq!(out.suggestions[3].name, "release/1.0");
    }

    #[test]
    fn object_without_name_is_invalid() {
        let out = normalize_json(json!({"description": "noname"}));
        assert!(out.suggestions.is_empty());
        assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::InvalidShape);
    }

    #[test]
    fn boolean_or_number_root_is_invalid() {
        for v in [json!(true), json!(42)] {
            let out = normalize_json(v);
            assert!(out.suggestions.is_empty());
            assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::InvalidShape);
        }
    }

    #[test]
    fn null_is_empty_output() {
        let out = normalize_json(Json::Null);
        assert!(out.suggestions.is_empty());
        assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::EmptyOutput);
    }

    #[test]
    fn empty_array_is_empty_output() {
        let out = normalize_json(json!([]));
        assert!(out.suggestions.is_empty());
        assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::EmptyOutput);
    }

    #[test]
    fn oversized_array_is_truncated_with_diagnostic() {
        let arr: Vec<_> = (0..MAX_SUGGESTIONS + 100)
            .map(|i| json!(format!("s{i}")))
            .collect();
        let out = normalize_json(Json::Array(arr));
        assert_eq!(out.suggestions.len(), MAX_SUGGESTIONS);
        assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::OversizedOutput);
    }

    #[test]
    fn oversized_name_is_diagnostic() {
        let big = "x".repeat(MAX_NAME_LEN + 1);
        let out = normalize_json(json!(big));
        assert!(out.suggestions.is_empty());
        assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::OversizedOutput);
    }

    #[test]
    fn array_with_function_is_invalid_shape() {
        // serde_json never sees a function — JSON.stringify renders it
        // as `null`. We reject mixed arrays element-by-element.
        let out = normalize_json(json!(["ok", null]));
        assert_eq!(out.suggestions.len(), 1);
        assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::InvalidShape);
    }
}
