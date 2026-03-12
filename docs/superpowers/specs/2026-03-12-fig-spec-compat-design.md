# Full Fig Spec Compatibility with Declarative Transform Pipeline

**Version:** 0.2.0
**Date:** 2026-03-12
**Status:** Approved

## Summary

Expand Ghost Complete from 34 hand-curated completion specs to full compatibility with the @withfig/autocomplete ecosystem (735+ specs). Introduce a declarative transform pipeline in Rust that handles dynamic completions (shell command execution + output transformation) without embedding a JavaScript runtime. Reserve optional JS runtime (QuickJS) as a future experimental feature flag for the remaining ~11% of specs that require full programmatic logic.

## Motivation

Ghost Complete has 46 GitHub stars in 11 days. The primary user base is Ghostty power users who want Fig-like completion back. The three biggest gaps compared to Fig:

1. **Breadth** — 34 specs vs 735. Users type `aws`, `terraform`, `systemctl` and get nothing.
2. **Information density** — Fig showed rich descriptions, argument types, required vs optional flags. Ghost Complete's popup is sparser.
3. **Dynamic completions** — Fig completed running container names, k8s pod names, git branches dynamically by executing shell commands at completion time.

All three are addressed by this design.

## Research Findings

### Fig Spec Ecosystem Analysis

The @withfig/autocomplete npm package contains **735 TypeScript spec files**. They compile to minified ES modules, not JSON. Analysis of all 735 specs:

| Category | Spec Count | % | Description |
|---|---|---|---|
| Pure static | ~460 | 63% | Subcommands, options, descriptions, `template: filepaths/folders` only |
| Script + simple split | ~100 | 14% | Run shell command, split output by newline = suggestions |
| Script + regex/filtering | ~55 | 7% | Split + regex extraction, error guards, column parsing |
| Script + JSON parsing | ~35 | 5% | Parse JSON lines from command output (e.g., docker `--format '{{ json . }}'`) |
| Dynamic script (context-dependent) | ~25 | 3% | Shell command varies based on user's current input tokens |
| Custom async generators | ~60 | 8% | Multiple sequential commands, HTTP API calls, conditional logic |
| **Total** | **~735** | **100%** | |

Note: Categories are approximate. Some specs span multiple categories; each is counted by its most complex generator.

**Key finding: ~77% of specs need zero JavaScript. With a transform pipeline, ~89% are covered.** The remaining ~11% (dynamic scripts + custom async) require programmatic logic deferred to the future JS runtime flag.

### postProcess Pattern Analysis

~90% of all `postProcess` functions across 735 specs are variations of 5 patterns:

1. **Split by newline + map to name** (dominant): `out.split("\n").map(line => ({ name: line }))`
2. **Split + JSON.parse per line**: `out.split("\n").map(line => JSON.parse(line)).map(i => ({ name: i.Field }))`
3. **Split + regex extraction**: `out.split("\n").map(line => { const m = line.match(/pattern/); return { name: m[1] }; })`
4. **Split + column extraction**: `out.split("\n").map(line => ({ name: line.substring(0, 7) }))`
5. **Error guard + split**: `if (out.includes("error:")) return []; return out.split("\n")...`

All five are expressible as declarative transform chains.

### JavaScript Runtime Options (for future experimental flag)

| | rquickjs (QuickJS) | boa_engine | deno_core |
|---|---|---|---|
| Binary size added | ~4 MB | ~15-27 MB | ~10-15 MB |
| Context startup | <300 μs | Low ms | Higher |
| Execution speed (V8 bench, higher=better) | 835 | 107 | 45,318 |
| ES6 arrow/map/filter | Yes | Yes | Yes |
| Language | C (Rust FFI) | Pure Rust | C++ (Rust FFI) |

**Recommendation for future JS flag:** rquickjs (QuickJS). 8x faster than boa, 4x smaller binary, <300μs startup fits within the 50ms keystroke budget. Battle-tested C engine (by Fabrice Bellard).

## Design

### Architecture Overview

```
Fig TypeScript Specs (735)
         │
         ▼
┌─────────────────────┐
│  Spec Converter CLI  │  (offline, build-time)
│  TypeScript → JSON   │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐     ┌──────────────────────┐
│  Static Spec Data   │     │  Transform Pipeline   │
│  (subcommands,      │     │  Definitions          │
│   options, args,    │     │  (script + transforms  │
│   descriptions)     │     │   in JSON)            │
└─────────┬───────────┘     └──────────┬───────────┘
          │                            │
          ▼                            ▼
┌──────────────────────────────────────────────────┐
│              gc-suggest Engine                     │
│                                                    │
│  Spec Provider ◄── reads JSON specs               │
│       │                                            │
│       ├── Static: subcommands/options/args         │
│       │   (existing code, works today)             │
│       │                                            │
│       ├── Template: filepaths/folders              │
│       │   (existing code, works today)             │
│       │                                            │
│       └── Dynamic: script + transform pipeline     │
│           (NEW — runs shell cmd, transforms output)│
│                                                    │
│  ┌────────────────────────────────────────┐       │
│  │  Transform Pipeline (Rust-native)      │       │
│  │                                        │       │
│  │  split_lines ──► filter_empty ──►      │       │
│  │  trim ──► regex_extract ──►            │       │
│  │  json_extract ──► column_extract ──►   │       │
│  │  error_guard ──► take(N)               │       │
│  │                                        │       │
│  │  ~10 composable Rust functions         │       │
│  │  Declared in JSON, executed natively   │       │
│  └────────────────────────────────────────┘       │
│                                                    │
│  ┌────────────────────────────────────────┐       │
│  │  [FUTURE] QuickJS Runtime (optional)   │       │
│  │  Feature flag: js-runtime              │       │
│  │  Handles: postProcess JS functions,    │       │
│  │  dynamic script functions              │       │
│  │  Does NOT handle: custom async         │       │
│  └────────────────────────────────────────┘       │
└──────────────────────────────────────────────────┘
```

### Component 1: Spec Converter (Offline Tool)

A build-time CLI tool that converts Fig's TypeScript specs to Ghost Complete's JSON format.

**Input:** `@withfig/autocomplete` npm package (TypeScript source files)
**Output:** JSON spec files compatible with Ghost Complete

**Conversion rules:**
- Static structure (subcommands, options, args, descriptions) → direct JSON mapping
- `template: "filepaths"` / `template: "folders"` → existing format (already supported)
- `script` (string array) + `postProcess` → `script` + `transforms` (pattern-matched)
- `script` (string array) + `splitOn` → `script` + `transforms: ["split_lines"]` (trivial)
- `script` (function) → mark as `requires_js: true`, include raw JS source for future QuickJS
- `custom` async generators → mark as `requires_js: true`, include raw JS source
- `loadSpec` (deferred loading) → inline the referenced sub-spec if available
- Fig icons → stripped (we use kind chars, not icons)

**Pattern matching for postProcess → transforms:**

The converter recognizes common postProcess patterns via AST analysis or regex on the compiled JS and emits equivalent transform chains:

| JS Pattern | Emitted Transforms |
|---|---|
| `out.split("\n").map(l => ({name: l}))` | `["split_lines", "filter_empty"]` |
| `out.split("\n").filter(Boolean).map(...)` | `["split_lines", "filter_empty", "trim"]` |
| `if (out.startsWith("X")) return []` | `[{"error_guard": {"starts_with": "X"}}, ...]` |
| `JSON.parse(line)` with field access | `[..., {"json_extract": {"name": "$.field"}}]` |
| `line.match(/regex/)` | `[..., {"regex_extract": {"pattern": "...", "name": 1}}]` |
| Unrecognized pattern | `requires_js: true` + raw JS preserved |

**The converter is NOT part of the ghost-complete binary.** It's a separate offline tool (or cargo subcommand) that users or CI runs to update specs. The binary only loads the resulting JSON files.

### Component 2: Extended Spec Format

Current Ghost Complete JSON spec format gains these new fields:

```json
{
  "name": "brew",
  "description": "The missing package manager for macOS",
  "subcommands": [
    {
      "name": "install",
      "description": "Install a formula or cask",
      "args": {
        "name": "formula",
        "generators": [{
          "script": ["brew", "formulae"],
          "transforms": ["split_lines", "filter_empty", "trim"],
          "cache": { "ttl_seconds": 300 }
        }]
      }
    },
    {
      "name": "services",
      "subcommands": [
        {
          "name": "stop",
          "args": {
            "name": "service",
            "generators": [{
              "script": ["brew", "services", "list"],
              "transforms": [
                "split_lines",
                "skip_first",
                "filter_empty",
                { "regex_extract": { "pattern": "^(\\S+)\\s+(\\S+)", "name": 1, "description": 2 } }
              ]
            }]
          }
        }
      ]
    }
  ]
}
```

**New spec fields:**

| Field | Type | Description |
|---|---|---|
| `generators[].script` | `string[]` | Shell command to execute (array form, no shell interpolation) |
| `generators[].transforms` | `Transform[]` | Ordered pipeline of transforms to apply to command stdout |
| `generators[].cache` | `CacheConfig?` | Optional TTL caching for generator results |
| `generators[].script_template` | `string[]` | Like `script` but supports `{prev_token}`, `{current_token}` substitution |
| `requires_js` | `bool` | Marks generators that need JS runtime (future feature) |
| `js_source` | `string?` | Raw JS function body (stored for future QuickJS execution) |

### Component 3: Transform Pipeline

A set of composable, pure Rust functions that process command output into suggestions.

**Core transforms:**

| Transform | Input | Output | Description |
|---|---|---|---|
| `split_lines` | `String` | `Vec<String>` | Split on `\n` |
| `split_on(delim)` | `String` | `Vec<String>` | Split on custom delimiter |
| `filter_empty` | `Vec<String>` | `Vec<String>` | Remove empty/whitespace-only lines |
| `trim` | `Vec<String>` | `Vec<String>` | Trim whitespace from each line |
| `skip_first` | `Vec<String>` | `Vec<String>` | Skip header line (common in CLI table output) |
| `skip(n)` | `Vec<String>` | `Vec<String>` | Skip first N lines |
| `take(n)` | `Vec<String>` | `Vec<String>` | Keep only first N lines |
| `error_guard` | `String` | `String` or empty | If output matches pattern, return no suggestions |
| `regex_extract` | `Vec<String>` | `Vec<Suggestion>` | Extract named fields via capture groups |
| `json_extract` | `Vec<String>` | `Vec<Suggestion>` | Parse each line as JSON, extract fields by path |
| `column_extract` | `Vec<String>` | `Vec<Suggestion>` | Extract by character position or whitespace-delimited column |
| `dedup` | `Vec<String>` | `Vec<String>` | Remove duplicate entries |

**Rust representation:**

```rust
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Transform {
    Named(NamedTransform),
    Parameterized(ParameterizedTransform),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NamedTransform {
    SplitLines,
    FilterEmpty,
    Trim,
    SkipFirst,
    Dedup,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ParameterizedTransform {
    SplitOn { delimiter: String },
    Skip { n: usize },
    Take { n: usize },
    ErrorGuard { starts_with: Option<String>, contains: Option<String> },
    RegexExtract { pattern: String, name: usize, description: Option<usize> },
    JsonExtract { name: String, description: Option<String> },
    ColumnExtract { name: Range, description: Option<Range> },
}
```

**Pipeline phases:**

The transform pipeline operates in two phases to handle the type transition from raw output to structured lines:

1. **Pre-split phase** (input: `String`, output: `String`): `error_guard` runs here — if the raw output matches an error pattern, the pipeline short-circuits and returns no suggestions.
2. **Post-split phase** (input: `Vec<String>`, output: `Vec<Suggestion>`): All other transforms (`filter_empty`, `trim`, `regex_extract`, `json_extract`, etc.) operate on the split lines.

The `split_lines` / `split_on` transform is the boundary between phases. If no split transform is specified, the entire output is treated as a single-element list.

**Execution model:**

1. `gc-suggest` encounters a generator with `script` + `transforms`
2. Execute shell command via `tokio::process::Command` with timeout (default 5s, configurable)
3. Capture stdout as `String`, discard stderr (logged at `tracing::debug` level)
4. Run pre-split transforms (error_guard)
5. Run split transform (split_lines / split_on)
6. Run post-split transforms (filter, trim, extract, etc.)
7. Output: `Vec<Suggestion>` ready for fuzzy ranking
8. If `cache` is configured, store results with TTL keyed by (spec_name, generator_index, cwd)

### Component 4: Shell Command Execution

Dynamic generators need to run shell commands at completion time. This requires:

**Async integration (critical architectural change):**

The current `SuggestionEngine::suggest_sync()` is fully synchronous, called from the handler in the PTY event loop. Dynamic generators are inherently async (shell command execution with timeout). Two-phase approach:

1. **Static suggestions returned immediately** — subcommands, options, descriptions, template-based completions continue to use the existing synchronous path. The popup renders with these instantly (<50ms).
2. **Dynamic suggestions computed asynchronously** — when a generator has a `script` field, spawn a `tokio::task` that runs the command, applies transforms, and sends results back via a `tokio::sync::mpsc` channel. When results arrive, merge into the popup and re-render.

This means the suggestion engine gains a new method: `suggest_dynamic(&self, ctx, cwd) -> mpsc::Receiver<Vec<Suggestion>>` that runs alongside the existing `suggest_sync()`. The handler orchestrates both: render static results immediately, then update when dynamic results arrive.

**UX for dynamic completion latency:**
- **0-50ms**: Static suggestions appear instantly (existing behavior)
- **50-200ms**: Dynamic results merge in, popup re-renders with additional items
- **200ms+**: Popup shows static results only; dynamic results merge when ready
- **Timeout (5s)**: Generator killed (SIGTERM, then SIGKILL after 1s), no dynamic results for that generator
- No explicit "loading" indicator in v0.2.0 — static results provide immediate feedback. Loading indicators are a v0.2.x polish item if users request it.

**Safety constraints:**
- Commands are arrays (no shell expansion/injection): `["brew", "list", "-1"]` executes directly via `execvp`, NOT `sh -c`
- Timeout: default 5 seconds, configurable per-generator or globally via `config.toml`. On timeout: SIGTERM, wait 1s, SIGKILL.
- Concurrency: max 3 generator commands in flight concurrently (allows multi-generator specs without blocking the whole system). Per-spec semaphore prevents a single spec from hogging all slots.
- Working directory: current directory as tracked by OSC 7
- Environment: inherit from shell, but strip `GHOST_COMPLETE_ACTIVE` to prevent recursive invocation. stderr is discarded (not forwarded to terminal — would corrupt output).

**`script_template` substitution safety:**
When `script_template` substitutes `{prev_token}` or `{current_token}` into command arrays, the substitution always produces a single argv element. User input like `; rm -rf /` becomes the literal string `"; rm -rf /"` as one argument — not interpreted by a shell. However, the substituted value IS passed as an argument to external commands, so specs should only use `script_template` with commands that safely handle arbitrary argument values.

**Performance:**
- Shell command execution is inherently slower than static completions
- Target: <200ms for generator commands (most CLI tools respond in <50ms)
- Caching mitigates repeat cost (e.g., `brew formulae` cached for 5 minutes)
- Transform pipeline itself: <1ms (Rust-native string processing)
- Total budget: well within the <500ms acceptable range for dynamic completions (static completions stay at <50ms)

### Component 5: Generator Caching

Many dynamic completions produce stable results (installed packages, available services, etc.) that don't change between keystrokes.

```json
"cache": {
  "ttl_seconds": 300,
  "cache_by_directory": true
}
```

**Cache key:** `(spec_name, generator_index, cwd_if_cache_by_directory)`
**Storage:** In-memory `HashMap` with expiry timestamps. No disk persistence.
**Invalidation:** TTL-based only. No filesystem watchers or event-based invalidation (YAGNI).

### Component 6: Spec Distribution

**Current (v0.1.x):** 34 specs embedded in the binary via `include_str!` and deployed to `~/.config/ghost-complete/specs/` during `ghost-complete install`. At runtime, specs are loaded from disk via `SpecStore::load_from_dir()`.

**New (v0.2.0):** Two-directory approach:

1. **Hand-written specs directory:** `~/.config/ghost-complete/specs/` — the 34 existing specs, deployed by `ghost-complete install` (unchanged from v0.1.x)
2. **Community specs directory:** `~/.config/ghost-complete/specs/community/` — auto-converted Fig specs, managed by `ghost-complete update-specs`
3. **Precedence:** Hand-written specs take priority. At load time, `SpecStore` loads from both directories. If the same command name exists in both, the hand-written version wins. This is a simple filename check — no in-binary loading or dual-source complexity.
4. **Update mechanism:** `ghost-complete update-specs` fetches pre-converted specs

**`ghost-complete update-specs` flow:**
1. Download pre-converted spec tarball from the latest GitHub Release (no npm dependency)
2. Extract to `~/.config/ghost-complete/specs/community/`
3. Report: N specs installed, M require JS (future), K skipped (hand-written version exists)

**Spec tarball build process (CI, not user-facing):**
1. CI job runs the spec converter against `@withfig/autocomplete` (requires Node.js in CI only)
2. Produces `ghost-complete-specs-vX.Y.Z.tar.gz` attached to each GitHub Release
3. Users never need Node.js or npm installed

**Backward compat with existing `GeneratorSpec`:**

The current `GeneratorSpec` struct uses `"type": "git_branches"` etc. for built-in Rust generators. This format is preserved. The new `script` + `transforms` fields are additive — they represent a new generator variant, not a replacement:

- `{ "type": "git_branches" }` → existing Rust-native git provider (unchanged)
- `{ "type": "filepaths" }` → existing filesystem provider (unchanged)
- `{ "script": [...], "transforms": [...] }` → NEW shell-execution generator

The engine checks: if `script` is present, use the transform pipeline. If `type` is present, use the existing built-in provider. Both can coexist in the same spec.

## Spec Coverage Tiers at Launch

| Tier | Coverage | Specs | Approach |
|---|---|---|---|
| **Hand-written (existing)** | Highest quality, Rust-native git/docker providers | 34 | Already done |
| **Static conversion** | Full subcommand/option/description tree | ~460 | Converter (zero JS) |
| **Transform pipeline** | Dynamic completions via script + transforms | ~190 | New Rust pipeline (simple split + regex + JSON) |
| **JS-required (deferred)** | Marked but non-functional until JS flag ships | ~85 | Future v0.3.0 (dynamic script + custom async) |
| **Total functional at launch** | | **~650 of 735** | **~89% coverage** |

Note: The 34 hand-written specs overlap with the converted set (both cover git, docker, etc.). The hand-written versions take precedence at runtime, so the effective unique command count is ~735 minus ~30 overlapping = ~705 unique commands covered.

## Performance Targets

| Operation | Target | Notes |
|---|---|---|
| Static completion (existing) | <50ms | No change |
| Transform pipeline execution | <1ms | Rust string processing |
| Shell command execution | <200ms | Async, with timeout |
| Cached generator hit | <1ms | HashMap lookup |
| Spec loading (700 JSON files) | <200ms at startup | Benchmark before shipping; lazy-load if >200ms |
| Memory for loaded specs | <15MB | Benchmark needed; lazy-load fallback if exceeds target |

## Testing Strategy

- **Transform pipeline unit tests:** Each transform function tested independently with known input/output
- **Integration tests:** Full generator flow (mock shell command → transforms → suggestions)
- **Spec converter tests:** Known Fig TypeScript → expected JSON output for representative specs
- **Regression tests:** Existing 234 tests remain unchanged (hand-written specs unaffected)
- **Benchmark tests:** Transform pipeline latency on realistic command output sizes (100, 1000, 10000 lines)

## Migration & Compatibility

- **v0.1.x specs remain untouched.** Existing hand-written JSON specs are fully forward-compatible. The existing `{ "type": "git_branches" }` generator format is preserved alongside the new `{ "script": [...], "transforms": [...] }` format.
- **No breaking config changes.** `config.toml` gains optional new fields (generator timeout, cache defaults) but existing configs work as-is.
- **Hand-written specs always win.** Specs in `~/.config/ghost-complete/specs/` take precedence over community specs in `specs/community/`. Users who never run `update-specs` see no change at all.
- **`suggest_sync` API changes.** The `SuggestionEngine` gains a new async method `suggest_dynamic()` alongside the existing `suggest_sync()`. Callers (handler.rs) need updating to orchestrate both. This is an internal API change — no user-facing impact.

## Future: Experimental JS Runtime (v0.3.0)

Reserved for a future version. Design notes for when we get there:

- **Runtime:** rquickjs (QuickJS via Rust FFI). ~4MB binary addition, <300μs context startup.
- **Feature flag:** `--features=js-runtime` at compile time, `experimental.js_runtime = true` in config at runtime.
- **Scope:** Execute `postProcess` JS functions and dynamic `script` functions from specs marked `requires_js: true`.
- **NOT in scope:** `custom` async generators (would require implementing Fig's `executeCommand` API — significant additional work).
- **Sandbox:** QuickJS context has no filesystem/network access. Only receives command stdout as input, returns suggestion array.

## Open Questions

1. **Spec converter implementation language:** Rust (parsing TypeScript AST with `tree-sitter`) vs Node.js script (can `require()` the compiled specs directly)? Node.js is simpler for the converter but only runs in CI — users never need it. Leaning Node.js for converter, Rust for everything else.
2. **Spec update frequency:** `update-specs` is manual-only for v0.2.0. Auto-update with configurable interval is a future consideration.
3. **Lazy spec loading:** If benchmarks show 700-file loading exceeds 200ms or 15MB, implement lazy loading: parse spec metadata (name, description) eagerly, full spec tree on first use. Decision deferred to benchmarking phase.
4. **Converter pattern coverage:** The converter's AST pattern matching will inevitably miss some `postProcess` patterns that ARE expressible as transforms but don't match the expected shape. Iteration on pattern coverage will happen post-launch as we discover edge cases. Specs that fail conversion gracefully degrade to `requires_js: true`.

## Summary of Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Spec format | Extend existing JSON with `script` + `transforms` | Backward-compatible, no new format to learn |
| Dynamic execution | Declarative transform pipeline in Rust | Covers ~89% of Fig specs, zero runtime overhead |
| JS runtime | Deferred to v0.3.0 under experimental flag | QuickJS (rquickjs), ~4MB, <300μs startup |
| Spec source | Convert from @withfig/autocomplete | 735 specs, maintained community, Fig-compatible |
| Distribution | External specs dir + update subcommand | Keeps binary lean, specs updatable independently |
| Caching | In-memory TTL-based | Simple, effective, no disk I/O |
| Version | 0.2.0 | Major feature addition warranting minor version bump |
