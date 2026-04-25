//! Dotted JSON-path parser for declarative spec extraction.
//!
//! Supports a small grammar rich enough to cover the Fig-spec
//! `json_extract` field selector:
//!
//! - `foo`              — single key segment
//! - `foo.bar.baz`      — nested object lookup
//! - `foo.0.bar`        — numeric array index segment
//! - `foo['bar'].baz`   — bracket-quoted key (single or double quotes)
//! - `foo["bar baz"]`   — quoted keys may contain spaces
//! - `$.foo.bar`        — leading `$.` JSONPath prefix is stripped
//!
//! Parsing is strict: malformed paths return `Err(message)` so a broken
//! spec is rejected at load time rather than silently no-op'ing at runtime.

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct JsonPath {
    segments: Vec<JsonPathSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonPathSegment {
    Key(String),
    Index(usize),
}

impl JsonPath {
    /// Parse a dotted path. Accepts an optional leading `$.` prefix.
    pub fn parse(s: &str) -> Result<Self, String> {
        if s.is_empty() {
            return Err("json path is empty".to_string());
        }
        // Strip leading JSONPath root.
        let s = s.strip_prefix("$.").unwrap_or(s);
        if s.is_empty() {
            return Err("json path is empty after $. prefix".to_string());
        }

        let mut segments = Vec::new();
        let bytes = s.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b'[' {
                // Bracket segment: [0] or ['key'] or ["key"]
                //
                // When the inner payload is quoted (`['k']` / `["k"]`) the
                // key itself may contain a `]` — e.g. `foo["a]b"]`. Walk
                // past the matching close-quote first so we don't
                // prematurely close on a `]` embedded in the key. For the
                // unquoted numeric-index form we fall back to the plain
                // "first `]`" search.
                let inner_start = i + 1;
                let end = if inner_start < bytes.len()
                    && (bytes[inner_start] == b'\'' || bytes[inner_start] == b'"')
                {
                    let quote = bytes[inner_start];
                    let after_open = inner_start + 1;
                    let close_quote = (after_open..bytes.len())
                        .find(|&j| bytes[j] == quote)
                        .ok_or_else(|| format!("unclosed quote at offset {i} in {s:?}"))?;
                    // The `]` must come immediately after the close-quote.
                    s[close_quote + 1..]
                        .find(']')
                        .map(|p| close_quote + 1 + p)
                        .ok_or_else(|| format!("unclosed '[' at offset {i} in {s:?}"))?
                } else {
                    s[inner_start..]
                        .find(']')
                        .map(|p| inner_start + p)
                        .ok_or_else(|| format!("unclosed '[' at offset {i} in {s:?}"))?
                };
                let inner = &s[i + 1..end];
                if inner.is_empty() {
                    return Err(format!("empty bracket segment at offset {i} in {s:?}"));
                }
                let (first, last) = (inner.as_bytes()[0], inner.as_bytes()[inner.len() - 1]);
                let seg = if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
                    if inner.len() < 2 {
                        return Err(format!("malformed quoted key at offset {i} in {s:?}"));
                    }
                    JsonPathSegment::Key(inner[1..inner.len() - 1].to_string())
                } else if let Ok(n) = inner.parse::<usize>() {
                    JsonPathSegment::Index(n)
                } else {
                    return Err(format!(
                        "bracket segment must be a quoted key or number, got {inner:?}"
                    ));
                };
                segments.push(seg);
                i = end + 1;
                // Optional trailing dot before the next dotted segment.
                if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                    if i == bytes.len() {
                        return Err(format!("trailing '.' at end of path {s:?}"));
                    }
                }
            } else {
                // Dotted segment: a run of chars until the next `.` or `[`.
                let start = i;
                while i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'[' {
                    i += 1;
                }
                let raw = &s[start..i];
                if raw.is_empty() {
                    return Err(format!("empty segment at offset {start} in {s:?}"));
                }
                let seg = if let Ok(n) = raw.parse::<usize>() {
                    JsonPathSegment::Index(n)
                } else {
                    JsonPathSegment::Key(raw.to_string())
                };
                segments.push(seg);
                if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                    if i == bytes.len() {
                        return Err(format!("trailing '.' at end of path {s:?}"));
                    }
                }
            }
        }

        if segments.is_empty() {
            return Err(format!("json path has no segments: {s:?}"));
        }

        Ok(JsonPath { segments })
    }

    /// Walk the path against a JSON value, returning `None` if any segment fails.
    pub fn lookup<'a>(&self, root: &'a serde_json::Value) -> Option<&'a serde_json::Value> {
        let mut cur = root;
        for seg in &self.segments {
            cur = match seg {
                JsonPathSegment::Key(k) => cur.get(k)?,
                JsonPathSegment::Index(i) => cur.get(*i)?,
            };
        }
        Some(cur)
    }

    /// True when this path is a single flat key (equivalent to `obj.get(key)`).
    pub fn is_flat(&self) -> bool {
        self.segments.len() == 1 && matches!(self.segments[0], JsonPathSegment::Key(_))
    }

    #[cfg(test)]
    fn segments(&self) -> &[JsonPathSegment] {
        &self.segments
    }
}

impl<'de> Deserialize<'de> for JsonPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct JsonPathVisitor;
        impl Visitor<'_> for JsonPathVisitor {
            type Value = JsonPath;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a dotted json-path string (e.g. \"foo.bar.baz\")")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<JsonPath, E> {
                JsonPath::parse(v).map_err(de::Error::custom)
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<JsonPath, E> {
                JsonPath::parse(&v).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_str(JsonPathVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_single_segment() {
        let p = JsonPath::parse("foo").unwrap();
        assert_eq!(p.segments(), &[JsonPathSegment::Key("foo".into())]);
        assert!(p.is_flat());
    }

    #[test]
    fn parses_dotted_chain() {
        let p = JsonPath::parse("foo.bar.baz").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key("bar".into()),
                JsonPathSegment::Key("baz".into()),
            ]
        );
        assert!(!p.is_flat());
    }

    #[test]
    fn parses_numeric_index() {
        let p = JsonPath::parse("foo.0.bar").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Index(0),
                JsonPathSegment::Key("bar".into()),
            ]
        );
    }

    #[test]
    fn parses_bracketed_single_quote() {
        let p = JsonPath::parse("foo['bar'].baz").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key("bar".into()),
                JsonPathSegment::Key("baz".into()),
            ]
        );
    }

    #[test]
    fn parses_bracketed_double_quote() {
        let p = JsonPath::parse("foo[\"bar\"].baz").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key("bar".into()),
                JsonPathSegment::Key("baz".into()),
            ]
        );
    }

    #[test]
    fn parses_bracketed_with_spaces() {
        let p = JsonPath::parse("foo[\"bar baz\"]").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key("bar baz".into()),
            ]
        );
    }

    #[test]
    fn test_quoted_bracket_key_containing_bracket() {
        // A quoted key is allowed to contain `]` — the bracket parser
        // must walk past the matching close-quote before searching for
        // the closing `]`. Previously `foo["a]b"]` closed prematurely at
        // the inner `]` and rejected the path.
        let p = JsonPath::parse("foo[\"a]b\"]").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key("a]b".into()),
            ]
        );

        // Same behavior for single quotes.
        let p = JsonPath::parse("foo['a]b'].c").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key("a]b".into()),
                JsonPathSegment::Key("c".into()),
            ]
        );

        // Looks up against a JSON value with a literal `]` in the key.
        let obj = serde_json::json!({"foo": {"a]b": 42}});
        let p = JsonPath::parse("foo[\"a]b\"]").unwrap();
        assert_eq!(p.lookup(&obj).and_then(|v| v.as_i64()), Some(42));
    }

    #[test]
    fn parses_with_leading_dollar() {
        let p = JsonPath::parse("$.foo.bar").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key("bar".into()),
            ]
        );
    }

    #[test]
    fn parses_bracketed_numeric_index() {
        // [3] is numeric, distinguishes from ['3'] which is a key.
        let p = JsonPath::parse("foo[3]").unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Index(3),
            ]
        );
    }

    #[test]
    fn rejects_empty() {
        assert!(JsonPath::parse("").is_err());
    }

    #[test]
    fn rejects_trailing_dot() {
        assert!(JsonPath::parse("foo.").is_err());
    }

    #[test]
    fn rejects_unclosed_bracket() {
        assert!(JsonPath::parse("foo[bar").is_err());
    }

    #[test]
    fn rejects_empty_bracket() {
        assert!(JsonPath::parse("foo[]").is_err());
    }

    #[test]
    fn rejects_leading_dot() {
        assert!(JsonPath::parse(".foo").is_err());
    }

    #[test]
    fn lookup_flat_matches_get() {
        let obj = json!({"name": "nginx", "status": "running"});
        let p = JsonPath::parse("name").unwrap();
        assert_eq!(p.lookup(&obj), obj.get("name"));
    }

    #[test]
    fn lookup_nested_object() {
        let obj = json!({"foo": {"bar": 42}});
        let p = JsonPath::parse("foo.bar").unwrap();
        assert_eq!(p.lookup(&obj).and_then(|v| v.as_i64()), Some(42));
    }

    #[test]
    fn lookup_array_index() {
        let obj = json!({"items": [{"name": "x"}, {"name": "y"}]});
        let p = JsonPath::parse("items.0.name").unwrap();
        assert_eq!(p.lookup(&obj).and_then(|v| v.as_str()), Some("x"));
    }

    #[test]
    fn lookup_missing_returns_none() {
        let obj = json!({"foo": 1});
        let p = JsonPath::parse("foo.bar.baz").unwrap();
        assert_eq!(p.lookup(&obj), None);
    }

    #[test]
    fn lookup_wrong_type_returns_none() {
        let obj = json!({"foo": "a string"});
        let p = JsonPath::parse("foo.0").unwrap();
        assert_eq!(p.lookup(&obj), None);
    }

    #[test]
    fn deserialize_from_json_string() {
        let p: JsonPath = serde_json::from_str(r#""foo.bar""#).unwrap();
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key("bar".into()),
            ]
        );
    }

    #[test]
    fn deserialize_invalid_returns_error() {
        let err = serde_json::from_str::<JsonPath>(r#""foo.""#).unwrap_err();
        assert!(err.to_string().contains("trailing"));
    }

    #[test]
    fn rejects_escape_in_quoted_key() {
        // The parser has NO escape syntax for quoted keys: a backslash
        // is a literal byte inside the key string, NOT an escape for
        // the following character. Pinning this contract blocks a
        // future parser refactor from silently adding escape support
        // (e.g. treating `\"` as a literal `"` inside the key or `\\`
        // as a literal `\`), which would be a breaking change for any
        // spec that relied on the current literal-byte semantics.
        //
        // Case 1: `foo["a\b"]` — the `\` is kept verbatim; key is the
        // three-byte string `a\b`, not the two-byte C-style `\b`
        // escape.
        let p = JsonPath::parse(r#"foo["a\b"]"#).expect("literal backslash is a valid key byte");
        assert_eq!(
            p.segments(),
            &[
                JsonPathSegment::Key("foo".into()),
                JsonPathSegment::Key(r"a\b".into()),
            ],
            "backslash must be treated as a literal byte, not an escape introducer"
        );
        match &p.segments()[1] {
            // Three bytes: `a`, `\`, `b` — if escape support snuck in,
            // the compiler would collapse this to two bytes.
            JsonPathSegment::Key(k) => assert_eq!(k.as_bytes(), b"a\\b"),
            other => panic!("expected Key segment, got {other:?}"),
        }

        // Case 2: `foo["a\"b"]` — `\"` does NOT escape the close-quote.
        // The parser finds the first literal `"` at offset 7 and closes
        // the segment there; the "inner" is the 6-byte slice `"a\"b"`
        // (with the outer quotes included), which the quoted-key arm
        // strips to the 4-byte key `a\"b`. If a future refactor adds
        // escape support, `\"` would be treated as a literal `"` and
        // the key would shrink to the 3-byte `a"b` — this assertion
        // catches that drift.
        let p =
            JsonPath::parse(r#"foo["a\"b"]"#).expect("current parser accepts literal backslash");
        match &p.segments()[1] {
            JsonPathSegment::Key(k) => assert_eq!(
                k.as_bytes(),
                br#"a\"b"#,
                "contract: `\\\"` is literal 4 bytes, not an escaped \
                 3-byte `a\"b`. If this ever equals `a\"b` as 3 bytes, \
                 escape support was added — update spec docs."
            ),
            other => panic!("expected Key segment, got {other:?}"),
        }
    }
}
