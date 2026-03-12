# Fig Spec Compatibility Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expand Ghost Complete from 34 hand-curated specs to ~650 functional specs (of 735 total) from the @withfig/autocomplete ecosystem, using a declarative transform pipeline for dynamic completions.

**Architecture:** Extended JSON spec format with `script` + `transforms` fields. Rust-native transform pipeline processes shell command output into suggestions. Async `suggest_dynamic()` runs alongside existing `suggest_sync()`. Node.js offline converter transforms Fig TypeScript specs to Ghost Complete JSON. All specs embedded in binary via `include_str!`.

**Tech Stack:** Rust (transform pipeline, engine, handler), Node.js (offline spec converter), tokio (async command execution), regex crate (regex_extract), serde_json (json_extract)

**Spec:** `docs/superpowers/specs/2026-03-12-fig-spec-compat-design.md`

---

## File Structure

### New Files

| File | Responsibility |
|---|---|
| `crates/gc-suggest/src/transform.rs` | Transform enum, custom Deserialize, pipeline executor, ordering validation |
| `crates/gc-suggest/src/script.rs` | Async shell command execution with timeout, safety constraints |
| `crates/gc-suggest/src/cache.rs` | In-memory TTL cache for generator results |
| `tools/fig-converter/package.json` | Node.js converter package |
| `tools/fig-converter/src/index.js` | Converter entry point |
| `tools/fig-converter/src/static-converter.js` | Static spec structure conversion |
| `tools/fig-converter/src/post-process-matcher.js` | postProcess → transforms pattern matching |
| `tools/fig-converter/src/native-map.js` | NATIVE_GENERATOR_MAP lookup table |

### Modified Files

| File | Changes |
|---|---|
| `crates/gc-suggest/src/specs.rs` | Extend `GeneratorSpec` with `script`, `transforms`, `cache`, `script_template`, `requires_js`, `js_source` fields |
| `crates/gc-suggest/src/engine.rs` | Add `suggest_dynamic()` async method, dispatch to script generators |
| `crates/gc-suggest/src/types.rs` | Add `SuggestionSource::Script` variant |
| `crates/gc-suggest/src/lib.rs` | Export new modules (`transform`, `script`, `cache`) |
| `crates/gc-suggest/Cargo.toml` | Add `regex` dependency |
| `crates/gc-buffer/src/context.rs` | Add `#[derive(Clone)]` to `CommandContext` |
| `crates/gc-pty/src/handler.rs` | Accept dynamic suggestions via channel, merge into popup |
| `crates/gc-pty/src/proxy.rs` | Spawn dynamic suggestion task, wire channel to handler |
| `crates/gc-config/src/lib.rs` | Add `generator_timeout_ms`, `cache` config sections |
| `crates/ghost-complete/src/main.rs` | Add `status` subcommand |
| `crates/ghost-complete/src/install.rs` | Update `EMBEDDED_SPECS` with converted specs |
| `Cargo.toml` | Bump workspace version to `0.2.0` |

### Parallelization Map

```
Chunk 1 (Foundation) ──────────────────────────────────────────┐
                                                                │
          ┌─────────────────────┬──────────────────────────────┤
          ▼                     ▼                              ▼
   Chunk 2 (Transforms)   Chunk 3 (Script+Cache)    Chunk 5 (Converter)
          │                     │                              │
          └──────────┬──────────┘                              │
                     ▼                                         │
          Chunk 4 (Engine+Handler Integration)                 │
                     │                                         │
                     └──────────────┬──────────────────────────┘
                                    ▼
                          Chunk 6 (Distribution+Status)
```

**Parallel opportunities:**
- Chunks 2, 3, 5 can all run as parallel subagents after Chunk 1 completes
- Chunk 4 depends on 2+3
- Chunk 6 depends on 4+5

---

## Chunk 1: Foundation — Extended Spec Format & Transform Types

### Task 1: Extend GeneratorSpec with new fields

**Files:**
- Modify: `crates/gc-suggest/src/specs.rs:51-55`
- Test: `crates/gc-suggest/src/specs.rs` (existing test module)

The current `GeneratorSpec` only has `generator_type: String`. We need to support both native generators (`{ "type": "git_branches" }`) and script generators (`{ "script": [...], "transforms": [...] }`).

- [ ] **Step 1: Write failing test for new spec format**

Add to `specs.rs` test module:

```rust
#[test]
fn test_deserialize_script_generator() {
    let spec: CompletionSpec = serde_json::from_str(r#"{
        "name": "brew",
        "args": [{
            "name": "formula",
            "generators": [{
                "script": ["brew", "formulae"],
                "transforms": ["split_lines", "filter_empty", "trim"],
                "cache": { "ttl_seconds": 300 }
            }]
        }]
    }"#).unwrap();
    let gen = &spec.args[0].generators[0];
    assert!(gen.script.is_some());
    assert!(gen.generator_type.is_none());
}

#[test]
fn test_deserialize_native_generator() {
    let spec: CompletionSpec = serde_json::from_str(r#"{
        "name": "git",
        "args": [{
            "generators": [{ "type": "git_branches" }]
        }]
    }"#).unwrap();
    let gen = &spec.args[0].generators[0];
    assert_eq!(gen.generator_type.as_deref(), Some("git_branches"));
    assert!(gen.script.is_none());
}

#[test]
fn test_deserialize_script_template_generator() {
    let spec: CompletionSpec = serde_json::from_str(r#"{
        "name": "test",
        "args": [{
            "generators": [{
                "script_template": ["cmd", "{prev_token}"],
                "transforms": ["split_lines"]
            }]
        }]
    }"#).unwrap();
    let gen = &spec.args[0].generators[0];
    assert_eq!(gen.script_template.as_ref().unwrap(), &vec!["cmd".to_string(), "{prev_token}".to_string()]);
}

#[test]
fn test_deserialize_requires_js_generator() {
    let spec: CompletionSpec = serde_json::from_str(r#"{
        "name": "test",
        "args": [{
            "generators": [{
                "requires_js": true,
                "js_source": "out.split('\\n').map(x => ({name: x}))"
            }]
        }]
    }"#).unwrap();
    let gen = &spec.args[0].generators[0];
    assert!(gen.requires_js);
    assert!(gen.js_source.is_some());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gc-suggest -- test_deserialize_script_generator test_deserialize_native_generator test_deserialize_script_template_generator test_deserialize_requires_js_generator`
Expected: FAIL (fields don't exist on GeneratorSpec)

- [ ] **Step 3: Extend GeneratorSpec**

Replace the `GeneratorSpec` struct in `specs.rs:51-55`:

```rust
use crate::transform::Transform;

#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub ttl_seconds: u64,
    #[serde(default)]
    pub cache_by_directory: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratorSpec {
    /// Native generator type (e.g., "git_branches", "git_tags", "git_remotes")
    #[serde(rename = "type")]
    pub generator_type: Option<String>,
    /// Shell command to execute (array form, no shell interpolation)
    pub script: Option<Vec<String>>,
    /// Like script but supports {prev_token}, {current_token} substitution
    pub script_template: Option<Vec<String>>,
    /// Ordered pipeline of transforms to apply to command stdout
    #[serde(default)]
    pub transforms: Vec<Transform>,
    /// Optional TTL caching for generator results
    pub cache: Option<CacheConfig>,
    /// Marks generators that need JS runtime (future feature)
    #[serde(default)]
    pub requires_js: bool,
    /// Raw JS function body (stored for future QuickJS execution)
    pub js_source: Option<String>,
}
```

Note: The `Transform` type import requires Task 2 to be complete. For this step, create a placeholder in `transform.rs`:

```rust
// crates/gc-suggest/src/transform.rs — placeholder
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Transform {
    Named(String),
}
```

And add `pub mod transform;` to `lib.rs`.

- [ ] **Step 4: Update resolve_spec to handle new GeneratorSpec**

The `resolve_spec` function currently pushes `gen.generator_type.clone()` to `generators: Vec<String>`. Change to push native generator types only, collecting script generators separately.

Update `SpecResolution`:

```rust
pub struct SpecResolution {
    pub subcommands: Vec<Suggestion>,
    pub options: Vec<Suggestion>,
    pub native_generators: Vec<String>,
    pub script_generators: Vec<GeneratorSpec>,
    pub wants_filepaths: bool,
    pub wants_folders_only: bool,
}
```

Update the generator collection loops in `resolve_spec()` to split by type:
- If `gen.generator_type.is_some()` → push type string to `native_generators`
- If `gen.script.is_some() || gen.script_template.is_some()` → push clone to `script_generators`
- If `gen.requires_js` → `tracing::info!("skipping generator requiring JS runtime in spec: {}", spec.name)`

- [ ] **Step 5: Update engine.rs to use new SpecResolution field names**

In `engine.rs`, rename `resolution.generators` → `resolution.native_generators` in the git dispatch loop (line ~146). The `script_generators` field is used later in Chunk 4.

- [ ] **Step 6: Run all tests**

Run: `cargo test -p gc-suggest`
Expected: ALL PASS (existing tests + new tests)

- [ ] **Step 7: Commit**

```bash
git add crates/gc-suggest/src/specs.rs crates/gc-suggest/src/transform.rs crates/gc-suggest/src/lib.rs crates/gc-suggest/src/engine.rs
git commit -m "feat: extend GeneratorSpec with script, transforms, cache fields for v0.2.0"
```

### Task 2: Define Transform enum with custom Deserialize

**Files:**
- Create: `crates/gc-suggest/src/transform.rs` (replace placeholder)
- Test: inline in `transform.rs`
- Modify: `crates/gc-suggest/Cargo.toml` (add `regex` dep)

- [ ] **Step 1: Add regex dependency**

Add to `crates/gc-suggest/Cargo.toml` under `[dependencies]`:
```toml
regex = "1"
```

- [ ] **Step 2: Write failing tests for Transform deserialization**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_named_transform() {
        let t: Transform = serde_json::from_str(r#""split_lines""#).unwrap();
        assert!(matches!(t, Transform::Named(NamedTransform::SplitLines)));
    }

    #[test]
    fn test_deserialize_filter_empty() {
        let t: Transform = serde_json::from_str(r#""filter_empty""#).unwrap();
        assert!(matches!(t, Transform::Named(NamedTransform::FilterEmpty)));
    }

    #[test]
    fn test_deserialize_parameterized_regex_extract() {
        let t: Transform = serde_json::from_str(r#"{"type": "regex_extract", "pattern": "^(\\S+)\\s+(\\S+)", "name": 1, "description": 2}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::RegexExtract { pattern, name, description }) => {
                assert_eq!(pattern, r"^(\S+)\s+(\S+)");
                assert_eq!(name, 1);
                assert_eq!(description, Some(2));
            }
            _ => panic!("expected RegexExtract"),
        }
    }

    #[test]
    fn test_deserialize_json_extract() {
        let t: Transform = serde_json::from_str(r#"{"type": "json_extract", "name": "$.Name", "description": "$.Status"}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::JsonExtract { name, description }) => {
                assert_eq!(name, "$.Name");
                assert_eq!(description.as_deref(), Some("$.Status"));
            }
            _ => panic!("expected JsonExtract"),
        }
    }

    #[test]
    fn test_deserialize_error_guard() {
        let t: Transform = serde_json::from_str(r#"{"type": "error_guard", "starts_with": "Error:"}"#).unwrap();
        match t {
            Transform::Parameterized(ParameterizedTransform::ErrorGuard { starts_with, contains }) => {
                assert_eq!(starts_with.as_deref(), Some("Error:"));
                assert!(contains.is_none());
            }
            _ => panic!("expected ErrorGuard"),
        }
    }

    #[test]
    fn test_deserialize_unknown_named_transform_errors() {
        let result: Result<Transform, _> = serde_json::from_str(r#""bogus_transform""#);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("split_lines"), "error should list valid names: {err}");
    }

    #[test]
    fn test_deserialize_transform_array() {
        let transforms: Vec<Transform> = serde_json::from_str(
            r#"["split_lines", "filter_empty", "trim", {"type": "regex_extract", "pattern": "^(.+)$", "name": 1}]"#
        ).unwrap();
        assert_eq!(transforms.len(), 4);
        assert!(matches!(transforms[0], Transform::Named(NamedTransform::SplitLines)));
        assert!(matches!(transforms[3], Transform::Parameterized(ParameterizedTransform::RegexExtract { .. })));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p gc-suggest -- transform::tests`
Expected: FAIL

- [ ] **Step 4: Implement Transform types with custom Deserialize**

Replace the placeholder `transform.rs` with the full implementation:

```rust
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use std::fmt;

use crate::types::Suggestion;

#[derive(Debug, Clone)]
pub enum Transform {
    Named(NamedTransform),
    Parameterized(ParameterizedTransform),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedTransform {
    SplitLines,
    FilterEmpty,
    Trim,
    SkipFirst,
    Dedup,
}

#[derive(Debug, Clone)]
pub enum ParameterizedTransform {
    SplitOn { delimiter: String },
    Skip { n: usize },
    Take { n: usize },
    ErrorGuard { starts_with: Option<String>, contains: Option<String> },
    RegexExtract { pattern: String, name: usize, description: Option<usize> },
    JsonExtract { name: String, description: Option<String> },
    ColumnExtract { column: usize, description_column: Option<usize> },
}

// Custom Deserialize for actionable error messages
impl<'de> Deserialize<'de> for Transform {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: Deserializer<'de> {
        struct TransformVisitor;

        impl<'de> Visitor<'de> for TransformVisitor {
            type Value = Transform;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a transform name (split_lines, filter_empty, trim, skip_first, dedup) or object (split_on, skip, take, error_guard, regex_extract, json_extract, column_extract)")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Transform, E> {
                match v {
                    "split_lines" => Ok(Transform::Named(NamedTransform::SplitLines)),
                    "filter_empty" => Ok(Transform::Named(NamedTransform::FilterEmpty)),
                    "trim" => Ok(Transform::Named(NamedTransform::Trim)),
                    "skip_first" => Ok(Transform::Named(NamedTransform::SkipFirst)),
                    "dedup" => Ok(Transform::Named(NamedTransform::Dedup)),
                    other => Err(de::Error::custom(format!(
                        "unknown transform name: \"{other}\". Valid names: split_lines, filter_empty, trim, skip_first, dedup"
                    ))),
                }
            }

            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Transform, M::Error> {
                // Delegate to ParameterizedTransform deserialization
                let param = ParameterizedTransform::deserialize(
                    de::value::MapAccessDeserializer::new(map)
                )?;
                Ok(Transform::Parameterized(param))
            }
        }

        deserializer.deserialize_any(TransformVisitor)
    }
}

// ParameterizedTransform uses internally-tagged deserialization via ParameterizedHelper
// Format: {"type": "regex_extract", "pattern": "...", "name": 1}
// The helper uses #[serde(tag = "type")] for clean dispatch, then converts to ParameterizedTransform.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum ParameterizedHelper {
    #[serde(rename = "split_on")]   SplitOn { delimiter: String },
    #[serde(rename = "skip")]       Skip { n: usize },
    #[serde(rename = "take")]       Take { n: usize },
    #[serde(rename = "error_guard")]ErrorGuard { starts_with: Option<String>, contains: Option<String> },
    #[serde(rename = "regex_extract")]RegexExtract { pattern: String, name: usize, description: Option<usize> },
    #[serde(rename = "json_extract")]JsonExtract { name: String, description: Option<String> },
    #[serde(rename = "column_extract")]ColumnExtract { column: usize, description_column: Option<usize> },
}

impl From<ParameterizedHelper> for ParameterizedTransform {
    fn from(h: ParameterizedHelper) -> Self {
        match h {
            ParameterizedHelper::SplitOn { delimiter } => ParameterizedTransform::SplitOn { delimiter },
            ParameterizedHelper::Skip { n } => ParameterizedTransform::Skip { n },
            ParameterizedHelper::Take { n } => ParameterizedTransform::Take { n },
            ParameterizedHelper::ErrorGuard { starts_with, contains } => ParameterizedTransform::ErrorGuard { starts_with, contains },
            ParameterizedHelper::RegexExtract { pattern, name, description } => ParameterizedTransform::RegexExtract { pattern, name, description },
            ParameterizedHelper::JsonExtract { name, description } => ParameterizedTransform::JsonExtract { name, description },
            ParameterizedHelper::ColumnExtract { column, description_column } => ParameterizedTransform::ColumnExtract { column, description_column },
        }
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p gc-suggest -- transform::tests`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
git add crates/gc-suggest/src/transform.rs crates/gc-suggest/Cargo.toml
git commit -m "feat: Transform enum with custom Deserialize for actionable error messages"
```

### Task 3: Transform ordering validation

**Files:**
- Modify: `crates/gc-suggest/src/transform.rs` (add `validate_pipeline`)
- Modify: `crates/gc-suggest/src/specs.rs` (call validation at load time)

- [ ] **Step 1: Write failing tests for validation**

Add to `transform.rs` tests:

```rust
#[test]
fn test_validate_valid_pipeline() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"["split_lines", "filter_empty", "trim"]"#
    ).unwrap();
    assert!(validate_pipeline(&transforms).is_ok());
}

#[test]
fn test_validate_error_guard_before_split() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"[{"type": "error_guard", "starts_with": "error"}, "split_lines", "filter_empty"]"#
    ).unwrap();
    assert!(validate_pipeline(&transforms).is_ok());
}

#[test]
fn test_validate_error_guard_after_split_fails() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"["split_lines", {"type": "error_guard", "starts_with": "error"}]"#
    ).unwrap();
    let err = validate_pipeline(&transforms).unwrap_err();
    assert!(err.contains("error_guard"), "error should mention error_guard: {err}");
}

#[test]
fn test_validate_post_split_before_split_fails() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"["filter_empty", "split_lines"]"#
    ).unwrap();
    let err = validate_pipeline(&transforms).unwrap_err();
    assert!(err.contains("filter_empty"), "error should mention the offending transform: {err}");
}

#[test]
fn test_validate_double_split_fails() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"["split_lines", {"type": "split_on", "delimiter": ","}]"#
    ).unwrap();
    let err = validate_pipeline(&transforms).unwrap_err();
    assert!(err.contains("split"), "error should mention split: {err}");
}

#[test]
fn test_validate_empty_pipeline_ok() {
    let transforms: Vec<Transform> = vec![];
    assert!(validate_pipeline(&transforms).is_ok());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gc-suggest -- transform::tests::test_validate`
Expected: FAIL

- [ ] **Step 3: Implement validate_pipeline**

Add to `transform.rs`:

```rust
/// Validate transform pipeline ordering at load time.
/// Returns Ok(()) if valid, Err(message) if invalid.
pub fn validate_pipeline(transforms: &[Transform]) -> Result<(), String> {
    let mut seen_split = false;

    for (i, t) in transforms.iter().enumerate() {
        let is_split = matches!(
            t,
            Transform::Named(NamedTransform::SplitLines)
                | Transform::Parameterized(ParameterizedTransform::SplitOn { .. })
        );
        let is_error_guard = matches!(
            t,
            Transform::Parameterized(ParameterizedTransform::ErrorGuard { .. })
        );
        let is_post_split = matches!(
            t,
            Transform::Named(
                NamedTransform::FilterEmpty
                    | NamedTransform::Trim
                    | NamedTransform::SkipFirst
                    | NamedTransform::Dedup
            ) | Transform::Parameterized(
                ParameterizedTransform::Skip { .. }
                    | ParameterizedTransform::Take { .. }
                    | ParameterizedTransform::RegexExtract { .. }
                    | ParameterizedTransform::JsonExtract { .. }
                    | ParameterizedTransform::ColumnExtract { .. }
            )
        );

        if is_split {
            if seen_split {
                return Err(format!(
                    "transform[{i}]: duplicate split transform; only one split_lines or split_on allowed"
                ));
            }
            seen_split = true;
        }

        if is_error_guard && seen_split {
            return Err(format!(
                "transform[{i}]: error_guard must appear before split_lines/split_on"
            ));
        }

        if is_post_split && !seen_split {
            let name = transform_name(t);
            return Err(format!(
                "transform[{i}]: {name} must appear after split_lines/split_on"
            ));
        }
    }

    Ok(())
}

fn transform_name(t: &Transform) -> &'static str {
    match t {
        Transform::Named(NamedTransform::SplitLines) => "split_lines",
        Transform::Named(NamedTransform::FilterEmpty) => "filter_empty",
        Transform::Named(NamedTransform::Trim) => "trim",
        Transform::Named(NamedTransform::SkipFirst) => "skip_first",
        Transform::Named(NamedTransform::Dedup) => "dedup",
        Transform::Parameterized(ParameterizedTransform::SplitOn { .. }) => "split_on",
        Transform::Parameterized(ParameterizedTransform::Skip { .. }) => "skip",
        Transform::Parameterized(ParameterizedTransform::Take { .. }) => "take",
        Transform::Parameterized(ParameterizedTransform::ErrorGuard { .. }) => "error_guard",
        Transform::Parameterized(ParameterizedTransform::RegexExtract { .. }) => "regex_extract",
        Transform::Parameterized(ParameterizedTransform::JsonExtract { .. }) => "json_extract",
        Transform::Parameterized(ParameterizedTransform::ColumnExtract { .. }) => "column_extract",
    }
}
```

- [ ] **Step 4: Wire validation into spec loading**

In `specs.rs`, after deserializing a spec in `load_spec()`, validate all generator pipelines:

```rust
fn load_spec(path: &Path) -> Result<CompletionSpec> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read spec file: {}", path.display()))?;
    let spec: CompletionSpec = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse spec file: {}", path.display()))?;

    // Validate transform pipelines at load time
    validate_spec_transforms(&spec, path)?;

    Ok(spec)
}
```

Implement `validate_spec_transforms` that walks all generators recursively and calls `validate_pipeline`. On error, log a warning with spec name and skip the malformed generator (don't fail the whole spec).

- [ ] **Step 5: Run all tests**

Run: `cargo test -p gc-suggest`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
git add crates/gc-suggest/src/transform.rs crates/gc-suggest/src/specs.rs
git commit -m "feat: transform pipeline ordering validation at spec load time"
```

---

## Chunk 2: Transform Pipeline Execution

**Parallelizable:** Can run as a subagent after Chunk 1.

### Task 4: Named transform functions

**Files:**
- Modify: `crates/gc-suggest/src/transform.rs`
- Test: inline

Implement the pure functions for: `split_lines`, `filter_empty`, `trim`, `skip_first`, `dedup`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn test_split_lines() {
    let input = "foo\nbar\nbaz\n";
    let result = apply_split_lines(input);
    assert_eq!(result, vec!["foo", "bar", "baz", ""]);
}

#[test]
fn test_filter_empty() {
    let input = vec!["foo".into(), "".into(), "bar".into(), "  ".into()];
    let result = apply_filter_empty(input);
    assert_eq!(result, vec!["foo", "bar"]);
}

#[test]
fn test_trim() {
    let input = vec!["  foo  ".into(), "bar\t".into()];
    let result = apply_trim(input);
    assert_eq!(result, vec!["foo", "bar"]);
}

#[test]
fn test_skip_first() {
    let input = vec!["HEADER".into(), "foo".into(), "bar".into()];
    let result = apply_skip_first(input);
    assert_eq!(result, vec!["foo", "bar"]);
}

#[test]
fn test_dedup() {
    let input = vec!["foo".into(), "bar".into(), "foo".into(), "baz".into()];
    let result = apply_dedup(input);
    assert_eq!(result, vec!["foo", "bar", "baz"]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gc-suggest -- transform::tests`
Expected: FAIL

- [ ] **Step 3: Implement named transforms**

```rust
fn apply_split_lines(input: &str) -> Vec<String> {
    input.split('\n').map(String::from).collect()
}

fn apply_filter_empty(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().filter(|l| !l.trim().is_empty()).collect()
}

fn apply_trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|l| l.trim().to_string()).collect()
}

fn apply_skip_first(lines: Vec<String>) -> Vec<String> {
    if lines.is_empty() { lines } else { lines.into_iter().skip(1).collect() }
}

fn apply_dedup(lines: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    lines.into_iter().filter(|l| seen.insert(l.clone())).collect()
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p gc-suggest -- transform::tests`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add crates/gc-suggest/src/transform.rs
git commit -m "feat: implement named transform functions (split, filter, trim, dedup)"
```

### Task 5: Parameterized transform functions

**Files:**
- Modify: `crates/gc-suggest/src/transform.rs`

Implement: `split_on`, `skip`, `take`, `error_guard`, `regex_extract`, `json_extract`, `column_extract`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn test_split_on() {
    let result = apply_split_on("a,b,c", ",");
    assert_eq!(result, vec!["a", "b", "c"]);
}

#[test]
fn test_skip_n() {
    let input = vec!["a".into(), "b".into(), "c".into(), "d".into()];
    let result = apply_skip(input, 2);
    assert_eq!(result, vec!["c", "d"]);
}

#[test]
fn test_take_n() {
    let input = vec!["a".into(), "b".into(), "c".into(), "d".into()];
    let result = apply_take(input, 2);
    assert_eq!(result, vec!["a", "b"]);
}

#[test]
fn test_error_guard_blocks() {
    let result = apply_error_guard("Error: something failed", Some("Error:"), None);
    assert!(result.is_none(), "error_guard should block output starting with Error:");
}

#[test]
fn test_error_guard_passes() {
    let result = apply_error_guard("normal output", Some("Error:"), None);
    assert!(result.is_some());
}

#[test]
fn test_regex_extract_name_and_description() {
    let lines = vec!["main    active".into(), "feature running".into()];
    let result = apply_regex_extract(&lines, r"^(\S+)\s+(\S+)", 1, Some(2));
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].text, "main");
    assert_eq!(result[0].description.as_deref(), Some("active"));
}

#[test]
fn test_regex_extract_name_only() {
    let lines = vec!["abc123".into(), "def456".into()];
    let result = apply_regex_extract(&lines, r"^(\w+)", 1, None);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].text, "abc123");
    assert!(result[0].description.is_none());
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
fn test_column_extract() {
    let lines = vec!["abc123  some description".into(), "def456  other desc".into()];
    let result = apply_column_extract(&lines, 0, Some(1));
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].text, "abc123");
    assert!(result[0].description.is_some());
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Implement parameterized transforms**

Key implementation notes:
- `apply_error_guard(output, starts_with, contains) -> Option<String>`: Returns `None` to abort pipeline, `Some(output)` to continue
- `apply_regex_extract(lines, pattern, name_group, desc_group) -> Vec<Suggestion>`: Uses `regex::Regex::new(pattern)`, extracts capture groups. Lines that don't match are silently skipped.
- `apply_json_extract(lines, name_path, desc_path) -> Vec<Suggestion>`: Parses each line as JSON, extracts fields by key name (not full JSONPath — just top-level field names like "Name", not "$.Name"). Strip the "$." prefix from the path if present.
- `apply_column_extract(lines, col, desc_col) -> Vec<Suggestion>`: Split each line by whitespace, extract columns by index.

All `Suggestion` results use `kind: SuggestionKind::Command` (generic), `source: SuggestionSource::Script`, `score: 0`.

- [ ] **Step 4: Run tests**

- [ ] **Step 5: Commit**

```bash
git add crates/gc-suggest/src/transform.rs crates/gc-suggest/src/types.rs
git commit -m "feat: implement parameterized transforms (regex, json, column extract)"
```

### Task 6: Pipeline executor

**Files:**
- Modify: `crates/gc-suggest/src/transform.rs`

The executor runs a `Vec<Transform>` against raw command output and produces `Vec<Suggestion>`.

- [ ] **Step 1: Write failing tests for the full pipeline**

```rust
#[test]
fn test_execute_pipeline_basic() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"["split_lines", "filter_empty", "trim"]"#
    ).unwrap();
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
        r#"[{"type": "error_guard", "starts_with": "Error:"}, "split_lines", "filter_empty"]"#
    ).unwrap();
    let output = "Error: command not found";
    let result = execute_pipeline(output, &transforms).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_execute_pipeline_with_error_guard_passes() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"[{"type": "error_guard", "starts_with": "Error:"}, "split_lines", "filter_empty"]"#
    ).unwrap();
    let output = "foo\nbar\n";
    let result = execute_pipeline(output, &transforms).unwrap();
    assert_eq!(result.len(), 2);
}

#[test]
fn test_execute_pipeline_regex_extract() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"["split_lines", "skip_first", "filter_empty", {"type": "regex_extract", "pattern": "^(\\S+)\\s+(\\S+)", "name": 1, "description": 2}]"#
    ).unwrap();
    let output = "NAME    STATUS\nnginx   running\nredis   stopped\n";
    let result = execute_pipeline(output, &transforms).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].text, "nginx");
    assert_eq!(result[0].description.as_deref(), Some("running"));
}

#[test]
fn test_execute_pipeline_json_extract() {
    let transforms: Vec<Transform> = serde_json::from_str(
        r#"["split_lines", "filter_empty", {"type": "json_extract", "name": "Name", "description": "Status"}]"#
    ).unwrap();
    let output = r#"{"Name":"nginx","Status":"running"}
{"Name":"redis","Status":"stopped"}"#;
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
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Implement execute_pipeline**

```rust
/// Execute a transform pipeline against raw command output.
/// Two-phase: pre-split (error_guard), then split, then post-split.
pub fn execute_pipeline(output: &str, transforms: &[Transform]) -> Result<Vec<Suggestion>, String> {
    // Phase 1: Pre-split transforms (error_guard)
    let mut raw = output.to_string();
    let mut split_idx = None;

    for (i, t) in transforms.iter().enumerate() {
        match t {
            Transform::Parameterized(ParameterizedTransform::ErrorGuard { starts_with, contains }) => {
                if let Some(result) = apply_error_guard(&raw, starts_with.as_deref(), contains.as_deref()) {
                    raw = result;
                } else {
                    return Ok(Vec::new()); // Error guard triggered, no suggestions
                }
            }
            Transform::Named(NamedTransform::SplitLines)
            | Transform::Parameterized(ParameterizedTransform::SplitOn { .. }) => {
                split_idx = Some(i);
                break;
            }
            _ => {} // validation already caught ordering errors
        }
    }

    // Phase 2: Split
    let mut lines = match split_idx {
        Some(i) => match &transforms[i] {
            Transform::Named(NamedTransform::SplitLines) => apply_split_lines(&raw),
            Transform::Parameterized(ParameterizedTransform::SplitOn { delimiter }) => {
                apply_split_on(&raw, delimiter)
            }
            _ => unreachable!(),
        },
        None => {
            // No split — treat whole output as single suggestion
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(Vec::new());
            }
            return Ok(vec![Suggestion {
                text: trimmed.to_string(),
                description: None,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Script,
                score: 0,
            }]);
        }
    };

    // Phase 3: Post-split transforms
    let post_start = split_idx.map_or(0, |i| i + 1);
    let mut suggestions: Option<Vec<Suggestion>> = None;

    for t in &transforms[post_start..] {
        match t {
            // Line-to-line transforms
            Transform::Named(NamedTransform::FilterEmpty) => lines = apply_filter_empty(lines),
            Transform::Named(NamedTransform::Trim) => lines = apply_trim(lines),
            Transform::Named(NamedTransform::SkipFirst) => lines = apply_skip_first(lines),
            Transform::Named(NamedTransform::Dedup) => lines = apply_dedup(lines),
            Transform::Parameterized(ParameterizedTransform::Skip { n }) => lines = apply_skip(lines, *n),
            Transform::Parameterized(ParameterizedTransform::Take { n }) => lines = apply_take(lines, *n),
            // Line-to-suggestion transforms (terminal)
            Transform::Parameterized(ParameterizedTransform::RegexExtract { pattern, name, description }) => {
                suggestions = Some(apply_regex_extract(&lines, pattern, *name, *description));
            }
            Transform::Parameterized(ParameterizedTransform::JsonExtract { name, description }) => {
                suggestions = Some(apply_json_extract(&lines, name, description.as_deref()));
            }
            Transform::Parameterized(ParameterizedTransform::ColumnExtract { column, description_column }) => {
                suggestions = Some(apply_column_extract(&lines, *column, *description_column));
            }
            _ => {} // error_guard/split already handled
        }
    }

    // If no extract transform was used, convert remaining lines to suggestions
    Ok(suggestions.unwrap_or_else(|| {
        lines.into_iter()
            .filter(|l| !l.is_empty())
            .map(|text| Suggestion {
                text,
                description: None,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Script,
                score: 0,
            })
            .collect()
    }))
}
```

- [ ] **Step 4: Run all transform tests**

Run: `cargo test -p gc-suggest -- transform::tests`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add crates/gc-suggest/src/transform.rs
git commit -m "feat: transform pipeline executor with two-phase execution"
```

---

## Chunk 3: Shell Command Execution & Caching

**Parallelizable:** Can run as a subagent after Chunk 1.

### Task 7: Async shell command execution

**Files:**
- Create: `crates/gc-suggest/src/script.rs`
- Modify: `crates/gc-suggest/src/lib.rs` (add `pub mod script;`)

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_script_echo() {
        let result = run_script(&["echo", "hello world"], Path::new("/tmp"), 5000).await.unwrap();
        assert_eq!(result.trim(), "hello world");
    }

    #[tokio::test]
    async fn test_run_script_timeout() {
        let result = run_script(&["sleep", "10"], Path::new("/tmp"), 100).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("timeout") || err.contains("timed out"), "expected timeout error: {err}");
    }

    #[tokio::test]
    async fn test_run_script_nonexistent_command() {
        let result = run_script(&["nonexistent_command_xyz"], Path::new("/tmp"), 5000).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_script_empty_command() {
        let result = run_script(&[], Path::new("/tmp"), 5000).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_script_multiline_output() {
        let result = run_script(&["printf", "foo\nbar\nbaz"], Path::new("/tmp"), 5000).await.unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn test_substitute_template_prev_token() {
        let template = vec!["cmd".to_string(), "--flag".to_string(), "{prev_token}".to_string()];
        let result = substitute_template(&template, Some("value"), None);
        assert_eq!(result, vec!["cmd", "--flag", "value"]);
    }

    #[test]
    fn test_substitute_template_current_token() {
        let template = vec!["cmd".to_string(), "{current_token}".to_string()];
        let result = substitute_template(&template, None, Some("partial"));
        assert_eq!(result, vec!["cmd", "partial"]);
    }

    #[test]
    fn test_substitute_template_length_limit() {
        let long = "a".repeat(2000);
        let template = vec!["cmd".to_string(), "{prev_token}".to_string()];
        let result = substitute_template(&template, Some(&long), None);
        assert!(result[1].len() <= 1024);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Implement script execution**

```rust
use std::path::Path;
use anyhow::{bail, Result};
use tokio::process::Command;

const MAX_SUBSTITUTION_LEN: usize = 1024;
const SHELL_METACHARACTERS: &[char] = &['|', ';', '&', '`', '$'];

/// Execute a shell command as an array (no shell interpolation).
/// Returns stdout as a String. Stderr is discarded (logged at debug level).
pub async fn run_script(argv: &[&str], cwd: &Path, timeout_ms: u64) -> Result<String> {
    if argv.is_empty() {
        bail!("empty script command");
    }

    let mut cmd = Command::new(argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.current_dir(cwd);
    // Strip GHOST_COMPLETE_ACTIVE to prevent recursive invocation
    cmd.env_remove("GHOST_COMPLETE_ACTIVE");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()
        .map_err(|e| anyhow::anyhow!("script execution failed for {:?}: {e}", argv))?;

    let output = match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait_with_output(),
    ).await {
        Ok(result) => result.map_err(|e| anyhow::anyhow!("script I/O error for {:?}: {e}", argv))?,
        Err(_) => {
            // Graceful shutdown: SIGTERM first, then SIGKILL after 1s
            let _ = child.start_kill(); // sends SIGKILL on Unix via tokio
            // Note: tokio's start_kill sends SIGKILL, not SIGTERM.
            // For proper SIGTERM-then-SIGKILL, use nix::sys::signal:
            // nix::sys::signal::kill(Pid::from_raw(child.id().unwrap() as i32), Signal::SIGTERM);
            // tokio::time::sleep(Duration::from_secs(1)).await;
            // let _ = child.kill().await;
            bail!("script timed out after {timeout_ms}ms: {:?}", argv);
        }
    };

    if !output.stderr.is_empty() {
        tracing::debug!(
            "script stderr for {:?}: {}",
            argv,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Substitute {prev_token} and {current_token} in a script template.
/// Each substitution produces a single argv element (no shell interpretation).
pub fn substitute_template(
    template: &[String],
    prev_token: Option<&str>,
    current_token: Option<&str>,
) -> Vec<String> {
    template.iter().map(|part| {
        let mut result = part.clone();
        if let Some(prev) = prev_token {
            let truncated = &prev[..prev.len().min(MAX_SUBSTITUTION_LEN)];
            result = result.replace("{prev_token}", truncated);
        } else {
            result = result.replace("{prev_token}", "");
        }
        if let Some(current) = current_token {
            let truncated = &current[..current.len().min(MAX_SUBSTITUTION_LEN)];
            result = result.replace("{current_token}", truncated);
        } else {
            result = result.replace("{current_token}", "");
        }
        // Warn on shell metacharacters in substituted values
        if result != *part && result.chars().any(|c| SHELL_METACHARACTERS.contains(&c)) {
            tracing::warn!("shell metacharacter in substituted script argument: {:?}", result);
        }
        result
    }).collect()
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p gc-suggest -- script::tests`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add crates/gc-suggest/src/script.rs crates/gc-suggest/src/lib.rs
git commit -m "feat: async shell command execution with timeout and template substitution"
```

### Task 8: Generator result caching

**Files:**
- Create: `crates/gc-suggest/src/cache.rs`
- Modify: `crates/gc-suggest/src/lib.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_suggestions() -> Vec<Suggestion> {
        vec![Suggestion {
            text: "test".into(),
            description: None,
            kind: SuggestionKind::Command,
            source: SuggestionSource::Script,
            score: 0,
        }]
    }

    #[test]
    fn test_cache_hit() {
        let cache = GeneratorCache::new();
        let key = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/tmp")));
        cache.insert(key.clone(), make_suggestions(), Duration::from_secs(300));
        let result = cache.get(&key);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn test_cache_miss() {
        let cache = GeneratorCache::new();
        let key = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/tmp")));
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn test_cache_expired() {
        let cache = GeneratorCache::new();
        let key = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/tmp")));
        // Insert with 0 TTL (already expired)
        cache.insert(key.clone(), make_suggestions(), Duration::from_secs(0));
        // Tiny sleep to ensure expiry
        std::thread::sleep(Duration::from_millis(10));
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn test_cache_different_cwd() {
        let cache = GeneratorCache::new();
        let key1 = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/tmp")));
        let key2 = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/home")));
        cache.insert(key1.clone(), make_suggestions(), Duration::from_secs(300));
        assert!(cache.get(&key1).is_some());
        assert!(cache.get(&key2).is_none());
    }

    #[test]
    fn test_cache_different_argv() {
        let cache = GeneratorCache::new();
        let key1 = CacheKey::new("docker", &["docker", "ps", "--format", "json"], Some(Path::new("/tmp")));
        let key2 = CacheKey::new("docker", &["docker", "images", "--format", "json"], Some(Path::new("/tmp")));
        cache.insert(key1.clone(), make_suggestions(), Duration::from_secs(300));
        assert!(cache.get(&key1).is_some());
        assert!(cache.get(&key2).is_none());
    }

    #[test]
    fn test_cache_script_template_different_prev_token_produces_different_keys() {
        // Verify that script_template generators with different {prev_token} values
        // produce different cache keys (spec: "Testing Strategy" item 6)
        let key1 = CacheKey::new("test", &["cmd", "--flag", "value_a"], Some(Path::new("/tmp")));
        let key2 = CacheKey::new("test", &["cmd", "--flag", "value_b"], Some(Path::new("/tmp")));
        assert_ne!(key1, key2, "different resolved argv should produce different cache keys");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Implement GeneratorCache**

```rust
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::types::Suggestion;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct CacheKey {
    spec_name: String,
    resolved_argv: Vec<String>,
    cwd: Option<String>,
}

impl CacheKey {
    pub fn new(spec_name: &str, argv: &[&str], cwd: Option<&Path>) -> Self {
        Self {
            spec_name: spec_name.into(),
            resolved_argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.map(|p| p.to_string_lossy().to_string()),
        }
    }

    pub fn from_strings(spec_name: &str, argv: &[String], cwd: Option<&Path>) -> Self {
        Self {
            spec_name: spec_name.into(),
            resolved_argv: argv.to_vec(),
            cwd: cwd.map(|p| p.to_string_lossy().to_string()),
        }
    }
}

struct CacheEntry {
    suggestions: Vec<Suggestion>,
    expires_at: Instant,
}

pub struct GeneratorCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
}

impl GeneratorCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, key: &CacheKey) -> Option<Vec<Suggestion>> {
        let entries = self.entries.lock().unwrap();
        entries.get(key).and_then(|entry| {
            if Instant::now() < entry.expires_at {
                Some(entry.suggestions.clone())
            } else {
                None
            }
        })
    }

    pub fn insert(&self, key: CacheKey, suggestions: Vec<Suggestion>, ttl: Duration) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(key, CacheEntry {
            suggestions,
            expires_at: Instant::now() + ttl,
        });
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p gc-suggest -- cache::tests`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add crates/gc-suggest/src/cache.rs crates/gc-suggest/src/lib.rs
git commit -m "feat: in-memory TTL cache for generator results"
```

---

## Chunk 4: Engine Integration & Handler Async Merge

**Sequential:** Depends on Chunks 2 + 3.

### Task 9: Add SuggestionSource::Script variant

**Files:**
- Modify: `crates/gc-suggest/src/types.rs`

- [ ] **Step 1: Add variant**

```rust
pub enum SuggestionSource {
    Filesystem,
    Git,
    History,
    Commands,
    Spec,
    Script,  // NEW: dynamic script-based generators
}
```

- [ ] **Step 2: Run all tests to check nothing breaks**

Run: `cargo test`
Expected: ALL PASS (exhaustive match warnings may appear — fix them)

- [ ] **Step 3: Commit**

```bash
git add crates/gc-suggest/src/types.rs
git commit -m "feat: add SuggestionSource::Script variant"
```

### Task 10: Implement suggest_dynamic() in SuggestionEngine

**Files:**
- Modify: `crates/gc-suggest/src/engine.rs`
- Modify: `crates/gc-suggest/src/lib.rs`

This is the core integration. `suggest_dynamic()` takes the same context as `suggest_sync()` but runs script generators asynchronously, applies transform pipelines, and returns dynamic suggestions.

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn test_suggest_dynamic_with_script_generator() {
    // Create a spec with a script generator
    let spec_json = r#"{"name": "test-dynamic", "args": [{"generators": [{"script": ["echo", "alpha\nbeta\ngamma"], "transforms": ["split_lines", "filter_empty"]}]}]}"#;
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("test-dynamic.json"), spec_json).unwrap();

    let engine = SuggestionEngine::new(dir.path()).unwrap();
    let ctx = CommandContext {
        command: Some("test-dynamic".into()),
        args: vec![],
        current_word: String::new(),
        word_index: 1,
        is_flag: false,
        is_long_flag: false,
        preceding_flag: None,
        in_pipe: false,
        in_redirect: false,
        quote_state: gc_buffer::QuoteState::None,
    };
    let results = engine.suggest_dynamic(&ctx, Path::new("/tmp"), 5000).await.unwrap();
    assert!(results.iter().any(|s| s.text == "alpha"), "expected 'alpha' in results: {results:?}");
    assert!(results.iter().any(|s| s.text == "beta"));
    assert!(results.iter().any(|s| s.text == "gamma"));
}
```

- [ ] **Step 2: Run test to verify it fails**

- [ ] **Step 3: Implement suggest_dynamic()**

Add to `SuggestionEngine`:

```rust
/// Run script-based generators asynchronously.
/// Returns dynamic suggestions (to be merged with static results from suggest_sync).
pub async fn suggest_dynamic(
    &self,
    ctx: &CommandContext,
    cwd: &Path,
    timeout_ms: u64,
) -> Result<Vec<Suggestion>> {
    if !self.providers_specs {
        return Ok(Vec::new());
    }

    let command = match &ctx.command {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };

    let spec = match self.spec_store.get(command) {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };

    let resolution = specs::resolve_spec(spec, ctx);

    if resolution.script_generators.is_empty() {
        return Ok(Vec::new());
    }

    let mut all_suggestions = Vec::new();
    // Max 3 concurrent generators (per-spec semaphore)
    let semaphore = Arc::new(tokio::sync::Semaphore::new(3));
    let mut handles = Vec::new();

    for gen in &resolution.script_generators {
        if gen.requires_js {
            tracing::info!("skipping generator requiring JS runtime in spec: {}", command);
            continue;
        }

        // Check cache first
        let argv = resolve_script_argv(gen, ctx);
        let cache_key = cache::CacheKey::from_strings(
            command,
            &argv,
            gen.cache.as_ref().map_or(Some(cwd), |c| {
                if c.cache_by_directory { Some(cwd) } else { None }
            }),
        );

        if let Some(cached) = self.generator_cache.get(&cache_key) {
            all_suggestions.extend(cached);
            continue;
        }

        let sem = semaphore.clone();
        let argv_owned = argv.clone();
        let transforms = gen.transforms.clone();
        let cache_config = gen.cache.clone();
        let cache = self.generator_cache.clone();
        let key = cache_key.clone();
        let cwd = cwd.to_path_buf();
        let spec_name = command.to_string();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let argv_refs: Vec<&str> = argv_owned.iter().map(|s| s.as_str()).collect();
            match script::run_script(&argv_refs, &cwd, timeout_ms).await {
                Ok(output) => {
                    match transform::execute_pipeline(&output, &transforms) {
                        Ok(suggestions) => {
                            // Cache if configured
                            if let Some(cc) = cache_config {
                                cache.insert(
                                    key,
                                    suggestions.clone(),
                                    std::time::Duration::from_secs(cc.ttl_seconds),
                                );
                            }
                            suggestions
                        }
                        Err(e) => {
                            tracing::warn!("transform pipeline error for {}: {}", spec_name, e);
                            Vec::new()
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("script generator error for {}: {}", spec_name, e);
                    Vec::new()
                }
            }
        }));
    }

    for handle in handles {
        match handle.await {
            Ok(suggestions) => all_suggestions.extend(suggestions),
            Err(e) => tracing::debug!("generator task panicked: {}", e),
        }
    }

    Ok(all_suggestions)
}
```

Also add `generator_cache: Arc<GeneratorCache>` to `SuggestionEngine` and initialize in `new()`:

```rust
use std::sync::Arc;
use crate::cache::GeneratorCache;

// In SuggestionEngine struct:
generator_cache: Arc<GeneratorCache>,

// In new():
generator_cache: Arc::new(GeneratorCache::new()),
```

Add helper function:

```rust
fn resolve_script_argv(gen: &specs::GeneratorSpec, ctx: &CommandContext) -> Vec<String> {
    if let Some(script) = &gen.script {
        script.clone()
    } else if let Some(template) = &gen.script_template {
        let prev = ctx.args.last().map(|s| s.as_str());
        let current = if ctx.current_word.is_empty() { None } else { Some(ctx.current_word.as_str()) };
        script::substitute_template(template, prev, current)
    } else {
        Vec::new()
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p gc-suggest`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add crates/gc-suggest/src/engine.rs crates/gc-suggest/src/lib.rs
git commit -m "feat: suggest_dynamic() for async script-based generators"
```

### Task 11: Handler async merge — channel and merge logic

**Files:**
- Modify: `crates/gc-pty/src/handler.rs`
- Modify: `crates/gc-pty/src/proxy.rs`

This is the most complex integration task. The handler needs to:
1. Call `suggest_sync()` immediately (existing behavior — static results)
2. Spawn `suggest_dynamic()` as a background task
3. When dynamic results arrive, merge them into the popup

- [ ] **Step 1: Add dynamic channel to InputHandler**

In `handler.rs`, add fields:

```rust
pub struct InputHandler {
    // ... existing fields ...
    /// Receiver for dynamic suggestions from background tasks
    dynamic_rx: Option<tokio::sync::mpsc::Receiver<Vec<Suggestion>>>,
    /// Timeout for script generators (ms)
    generator_timeout_ms: u64,
}
```

- [ ] **Step 2: Add method to receive and merge dynamic results**

```rust
/// Check for pending dynamic results and merge into current suggestions.
/// Returns true if the popup needs re-rendering.
pub fn try_merge_dynamic(&mut self) -> bool {
    let rx = match self.dynamic_rx.as_mut() {
        Some(rx) => rx,
        None => return false,
    };

    match rx.try_recv() {
        Ok(dynamic) if !dynamic.is_empty() => {
            if self.visible {
                // Append below static results (preserve user's navigation position)
                self.suggestions.extend(dynamic);
                true // needs re-render
            } else {
                false // popup dismissed before arrival — discard
            }
        }
        _ => false,
    }
}
```

- [ ] **Step 3: Modify trigger() to spawn dynamic task**

In `trigger()`, after calling `suggest_sync()` and rendering static results, spawn the dynamic task:

```rust
pub fn trigger(
    &mut self,
    parser: &Arc<Mutex<TerminalParser>>,
    stdout: &mut dyn Write,
) {
    // ... existing buffer/cursor/cwd extraction ...
    let ctx = parse_command_context(&buffer, cursor);

    // Static results — immediate
    match self.engine.suggest_sync(&ctx, &cwd) {
        Ok(suggestions) if !suggestions.is_empty() => {
            self.suggestions = suggestions;
            self.overlay.reset();
            self.visible = true;
            self.render_at(stdout, cursor_row, cursor_col, screen_rows, screen_cols);
        }
        _ => {
            if self.visible {
                self.dismiss(stdout);
            }
        }
    }

    // Dynamic results — async (fire and forget, merged on arrival)
    // Cancel any previous dynamic task by replacing the receiver
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    self.dynamic_rx = Some(rx);

    let engine = self.engine.clone(); // engine needs to be cloneable or Arc'd
    let ctx_owned = ctx.clone();
    let cwd_owned = cwd.clone();
    let timeout = self.generator_timeout_ms;

    tokio::spawn(async move {
        match engine.suggest_dynamic(&ctx_owned, &cwd_owned, timeout).await {
            Ok(suggestions) if !suggestions.is_empty() => {
                let _ = tx.send(suggestions).await;
            }
            _ => {}
        }
    });
}
```

**Critical:** `SuggestionEngine` needs to be `Send + Sync` and clonable (or wrapped in `Arc`). The engine is currently stored directly in `InputHandler`. Change to `Arc<SuggestionEngine>`:

```rust
pub struct InputHandler {
    engine: Arc<SuggestionEngine>,
    // ...
}
```

And `CommandContext` needs `Clone`. Check if it already derives Clone — if not, add `#[derive(Clone)]` to it in `gc-buffer`.

- [ ] **Step 4: Add dynamic merge polling to proxy.rs**

In proxy.rs, add a periodic check in the `tokio::select!` loop (Task C / signal handler section). After processing PTY output in Task B, check for dynamic results:

```rust
// After the buffer_dirty / cwd_dirty handling in Task B:
{
    let mut h = handler_for_stdout.lock().unwrap();
    if h.try_merge_dynamic() {
        // Re-render with merged results
        let mut render_buf = Vec::new();
        h.render(&parser_for_stdout, &mut render_buf);
        if !render_buf.is_empty() {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&render_buf);
            let _ = stdout.flush();
        }
    }
}
```

Note: `try_merge_dynamic()` is non-blocking (uses `try_recv`), so this is safe in the blocking Task B. The dynamic results will be picked up on the next PTY output cycle. For better latency, consider a separate polling task, but this is sufficient for v0.2.0.

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
git add crates/gc-pty/src/handler.rs crates/gc-pty/src/proxy.rs crates/gc-suggest/src/engine.rs crates/gc-buffer/src/context.rs
git commit -m "feat: async dynamic suggestion merge — static results first, dynamic merge on arrival"
```

### Task 12: Config changes for generators

**Files:**
- Modify: `crates/gc-config/src/lib.rs`

- [ ] **Step 1: Add generator config fields**

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SuggestConfig {
    pub max_results: usize,
    pub max_history_entries: usize,
    pub providers: ProvidersConfig,
    pub generator_timeout_ms: u64,    // NEW
}

impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            max_results: 50,
            max_history_entries: 10_000,
            providers: ProvidersConfig::default(),
            generator_timeout_ms: 5000, // 5 seconds
        }
    }
}
```

- [ ] **Step 2: Wire timeout through handler**

Pass `config.suggest.generator_timeout_ms` to `InputHandler` via a new builder method:

```rust
pub fn with_generator_timeout(mut self, timeout_ms: u64) -> Self {
    self.generator_timeout_ms = timeout_ms;
    self
}
```

Call it in `proxy.rs` during handler initialization.

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 4: Commit**

```bash
git add crates/gc-config/src/lib.rs crates/gc-pty/src/handler.rs crates/gc-pty/src/proxy.rs
git commit -m "feat: generator_timeout_ms config field (default 5s)"
```

---

## Chunk 5: Node.js Spec Converter

**Parallelizable:** Can run as a subagent after Chunk 1. Completely independent of Rust code changes.

### Task 13: Converter scaffold and static conversion

**Files:**
- Create: `tools/fig-converter/package.json`
- Create: `tools/fig-converter/src/index.js`
- Create: `tools/fig-converter/src/static-converter.js`

- [ ] **Step 1: Create package.json**

```json
{
  "name": "fig-converter",
  "version": "0.1.0",
  "private": true,
  "description": "Converts @withfig/autocomplete specs to Ghost Complete JSON format",
  "main": "src/index.js",
  "scripts": {
    "convert": "node src/index.js",
    "test": "node --test src/*.test.js"
  },
  "dependencies": {
    "@withfig/autocomplete": "^2"
  }
}
```

- [ ] **Step 2: Implement static converter**

`src/static-converter.js` — converts a Fig spec object to Ghost Complete JSON:

- Walk `subcommands` array recursively → map to `{ name, description, subcommands, options, args }`
- Walk `options` → map to `{ name: [names], description, args }`
- Walk `args` → map to `{ name, description, template, generators }`
- `template: "filepaths"` and `template: "folders"` pass through
- Static generators (no script/postProcess) → pass `type` if present

- [ ] **Step 3: Write tests for static conversion**

Create `src/static-converter.test.js` with node:test. Test with a simple hand-crafted Fig spec object and verify the output JSON matches expected format.

- [ ] **Step 4: Run tests**

Run: `cd tools/fig-converter && npm test`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add tools/fig-converter/
git commit -m "feat: fig-converter scaffold with static spec conversion"
```

### Task 14: postProcess pattern matching

**Files:**
- Create: `tools/fig-converter/src/post-process-matcher.js`
- Create: `tools/fig-converter/src/post-process-matcher.test.js`

This is the most complex part of the converter. It analyzes the `postProcess` function body (as a string via `.toString()`) and emits equivalent `transforms` arrays.

- [ ] **Step 1: Implement pattern matchers**

Match these patterns (from the spec's research findings):

1. **Split by newline**: `out.split("\n")` → `["split_lines", "filter_empty"]`
2. **Split + filter**: `out.split("\n").filter(Boolean)` → `["split_lines", "filter_empty"]`
3. **Split + JSON.parse**: `JSON.parse(line)` with field access → `["split_lines", "filter_empty", {"type": "json_extract", ...}]`
4. **Split + regex**: `line.match(/pattern/)` → `["split_lines", "filter_empty", {"type": "regex_extract", ...}]`
5. **Error guard + split**: `if (out.includes("error"))` → `[{"type": "error_guard", ...}, "split_lines", ...]`
6. **Unrecognized** → `requires_js: true`, preserve raw JS source

Strategy: Convert the `postProcess` function to a string, then use regex/string matching on the function body. This is intentionally heuristic — unrecognized patterns gracefully degrade to `requires_js: true`.

- [ ] **Step 2: Write tests**

Test each pattern with representative function bodies from real Fig specs.

- [ ] **Step 3: Run tests**

- [ ] **Step 4: Commit**

```bash
git add tools/fig-converter/src/post-process-matcher.js tools/fig-converter/src/post-process-matcher.test.js
git commit -m "feat: postProcess pattern matching (5 patterns) in fig-converter"
```

### Task 15: NATIVE_GENERATOR_MAP and full conversion

**Files:**
- Create: `tools/fig-converter/src/native-map.js`
- Modify: `tools/fig-converter/src/index.js` (full pipeline)

- [ ] **Step 1: Implement native generator map**

```javascript
// Maps (command, script_command) → native generator type
const NATIVE_GENERATOR_MAP = {
    'git branch': { type: 'git_branches' },
    'git tag': { type: 'git_tags' },
    'git remote': { type: 'git_remotes' },
};

function matchNativeGenerator(specName, scriptArgv) {
    const key = scriptArgv.slice(0, 2).join(' ');
    return NATIVE_GENERATOR_MAP[key] || null;
}
```

- [ ] **Step 2: Wire full conversion pipeline in index.js**

The main converter:
1. Load `@withfig/autocomplete` specs via `require()`
2. For each spec:
   a. Convert static structure (static-converter.js)
   b. **Handle `loadSpec` references** — if a spec has `loadSpec: "otherSpec"` or `loadSpec: { specName }`, resolve the referenced spec and inline its subcommand tree. This is critical for `aws`, `terraform`, `docker-compose` which use deferred loading extensively. Walk the module's exports to find the referenced spec object. If the reference cannot be resolved (e.g., dynamic loadSpec function), mark as `requires_js: true`.
   c. For each generator with `script` + `postProcess`:
      - Check NATIVE_GENERATOR_MAP first
      - If match → emit `{ "type": "..." }`
      - Else → run postProcess pattern matcher
      - If pattern matched → emit `{ "script": [...], "transforms": [...] }`
      - If unrecognized → emit `{ "requires_js": true, "js_source": "..." }`
   d. For each generator with `script` + `splitOn`:
      - Emit `{ "script": [...], "transforms": ["split_lines"] }`
   e. Handle `script` as function → `requires_js: true`
   f. Handle `custom` async generators → `requires_js: true`
3. Write JSON to `specs/` directory (or stdout)

- [ ] **Step 3: Test with a few real Fig specs**

Run the converter on `git`, `docker`, `brew` specs and verify output:
- git.json should have native generators for branches/tags/remotes
- docker.json should have script + json_extract generators
- brew.json should have script + split_lines generators

**Critical test: Native generator map priority.** Verify that the converter emits `{ "type": "git_branches" }` (not `{ "script": ["git", "branch"] }`) for all commands in NATIVE_GENERATOR_MAP. Write a test that converts the git spec and asserts that branch/tag/remote generators use the native `type` field, not the `script` field.

**Critical test: loadSpec inlining.** Verify that a spec with `loadSpec: "otherSpec"` has the referenced spec's subcommands inlined in the output JSON. Test with a known loadSpec user (e.g., `docker-compose` references `docker`).

- [ ] **Step 4: Run full conversion**

Run: `cd tools/fig-converter && node src/index.js --output ../../specs/`

Verify output count: should produce ~735 JSON files.

- [ ] **Step 5: Commit**

```bash
git add tools/fig-converter/ specs/
git commit -m "feat: full fig-converter with NATIVE_GENERATOR_MAP and 735 converted specs"
```

---

## Chunk 6: Distribution, Status & Final Integration

**Sequential:** Depends on Chunks 4 + 5.

### Task 16: Update embedded specs

**Files:**
- Modify: `crates/ghost-complete/src/install.rs`

The converter produces ~735 JSON specs. All need to be embedded via `include_str!`.

- [ ] **Step 1: Generate the EMBEDDED_SPECS list**

Write a script (or do manually) that generates the `include_str!` entries for all JSON files in `specs/`. Order alphabetically.

- [ ] **Step 2: Replace EMBEDDED_SPECS in install.rs**

Replace the existing 34-entry list with the full ~735-entry list.

- [ ] **Step 3: Build and verify**

Run: `cargo build`
Expected: Compiles (may take longer due to more embedded strings). Check binary size increase (~10-15MB is expected and acceptable).

- [ ] **Step 4: Commit**

```bash
git add crates/ghost-complete/src/install.rs specs/
git commit -m "feat: embed all 735 converted specs via include_str!"
```

### Task 17: ghost-complete status subcommand

**Files:**
- Modify: `crates/ghost-complete/src/main.rs`
- Create: `crates/ghost-complete/src/status.rs`

- [ ] **Step 1: Add status subcommand to CLI**

In main.rs, add `Status` variant to the subcommand enum. The status command reports:
- Total specs loaded
- Fully functional (no `requires_js` generators)
- Partially functional (has `requires_js` generators — static completions work, dynamic don't)
- List of commands with `requires_js` generators

- [ ] **Step 2: Implement status reporting**

```rust
pub fn run_status(config: &GhostConfig) -> Result<()> {
    let spec_dirs = resolve_spec_dirs(&config.paths.spec_dirs);
    let result = SpecStore::load_from_dir(&spec_dirs[0])?;
    // Walk all specs, check generators for requires_js
    // Print colored summary
}
```

The SpecStore needs a method to iterate all specs: `pub fn iter(&self) -> impl Iterator<Item = (&str, &CompletionSpec)>`.

- [ ] **Step 3: Test**

Run: `cargo run -- status`
Expected: Prints spec coverage summary

- [ ] **Step 4: Commit**

```bash
git add crates/ghost-complete/src/main.rs crates/ghost-complete/src/status.rs
git commit -m "feat: ghost-complete status subcommand for spec coverage reporting"
```

### Task 18: Version bump and final checks

**Files:**
- Modify: `Cargo.toml` (workspace version)

- [ ] **Step 1: Bump version to 0.2.0**

In workspace `Cargo.toml`:
```toml
version = "0.2.0"
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings

- [ ] **Step 4: Run fmt check**

Run: `cargo fmt --check`
Expected: Clean

- [ ] **Step 5: Benchmark spec loading with 735 specs**

```bash
# Time spec loading at startup
RUST_LOG=debug cargo run -- /bin/true 2>&1 | grep -i "loaded spec\|startup"
```

Measure time from process start to first suggestion readiness. If spec loading exceeds 50ms, lazy loading must be implemented before release (see spec: "Spec metadata loading (700 specs): <50ms at startup"). If under 50ms, lazy loading is deferred to v0.2.x.

- [ ] **Step 6: Verify binary works**

```bash
cargo build --release
cp target/release/ghost-complete ~/.cargo/bin/
codesign -f -s - ~/.cargo/bin/ghost-complete
ghost-complete install
ghost-complete status
ghost-complete validate-specs
```

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml
git commit -m "chore: bump version to 0.2.0"
```

---

## Implementation Notes for Subagent Workers

### Chunk Dependencies (Critical)

- **Chunk 1 MUST complete before any other chunk starts** — it defines the types everything else uses
- **Chunks 2, 3, 5 are independent** — can run as parallel subagents
- **Chunk 4 depends on Chunks 2 + 3** — engine integration needs transform pipeline and script execution
- **Chunk 6 depends on Chunks 4 + 5** — needs converted specs and working engine

### Testing Commands

- Single crate: `cargo test -p gc-suggest`
- Single test: `cargo test -p gc-suggest -- test_name`
- All: `cargo test`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Format: `cargo fmt --check`
- Converter: `cd tools/fig-converter && npm test`

### Key Design Decisions (from spec, confirmed with user)

1. **Converter language:** Node.js (can `require()` Fig specs directly)
2. **script_template safety:** Option C (emit all with mitigations — length limit + metacharacter warning)
3. **Distribution:** All specs embedded via `include_str!` (no download mechanism)
4. **JS runtime:** Deferred to v0.3.0 (QuickJS under feature flag)
5. **Caching:** In-memory TTL only (no disk persistence)
6. **Dynamic merge UX:** Append below static, re-rank on next keystroke only. "Re-rank on next keystroke" is naturally handled by the trigger mechanism: when the user types the next character, `trigger()` fires fresh `suggest_sync()` + `suggest_dynamic()` calls, producing a completely new ranked result set. The old merged results are replaced entirely. No special interleaving code needed.
7. **Loading:** Eager full parse for v0.2.0. If startup benchmark (Task 18 Step 5) shows >50ms with 735 specs, lazy loading becomes a blocking prerequisite before release.

### Files You Must Not Break

- All 234 existing tests must continue to pass
- `suggest_sync()` must remain synchronous and <50ms
- Existing spec format must remain backward-compatible (new fields are all optional)
- `config.toml` must remain backward-compatible (new fields have defaults)
- `ghost-complete install` must remain idempotent
