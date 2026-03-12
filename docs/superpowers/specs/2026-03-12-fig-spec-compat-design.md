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

### Dynamic Generator Audit

Full audit of Fig's dynamic generators across major CLI tools, classified by whether they can be implemented as Rust-native providers (file parsing, no external binary) or must remain as script-based execution.

**Current Rust-native generators in Ghost Complete (v0.1.x): 3 total**

| Generator | Implementation | Used in specs |
|---|---|---|
| `git_branches` | `git branch --format=%(refname:short)` | git (checkout, push, pull) |
| `git_tags` | `git tag --list` | none (defined but unused) |
| `git_remotes` | `git remote` | git (push, pull) |

Plus 2 templates (not generators): `filepaths` and `folders`.

**Fig generator landscape by tool:**

#### Git — 14 generators in Fig, 3 currently native

| Generator | Fig's shell command | Native? | Rationale |
|---|---|---|---|
| branches | `git branch --no-color` | **Already native** | `git_branches` |
| tags | `git tag --list` | **Already native** | `git_tags` |
| remotes | `git remote -v` | **Already native** | `git_remotes` |
| stashes | `git stash list` | Could be native | Parse `.git/logs/refs/stash` |
| aliases | `git config --get-regexp ^alias.` | Could be native | Parse `.gitconfig` / `.git/config` |
| commits | `git log --oneline` | Script | Needs git binary for object DB |
| revs | `git rev-list --all --oneline` | Script | Needs git binary |
| staged files | `git status --short` | Script | Needs git binary for index |
| unstaged files | `git diff --name-only` | Script | Needs git binary |
| files for staging | `git status --short` | Script | Needs git binary |
| changed tracked files | `git status --short` (context-dependent) | Script | Needs git binary + context |
| local+remote branches | `git branch -a` | Script (or extend native) | Could extend `git_branches` |
| treeish | `git diff --cached --name-only` | Script | Needs git binary |
| context-dependent branches | Conditional on `-r` flag | `requires_js` likely | Token context logic |

#### Docker — 18+ generators, NONE can be native

All require the Docker daemon. No file to parse. Perfect for script + `json_extract` (Docker's `--format "{{ json . }}"` maps directly to the transform pipeline).

| Generator | Fig's command | Transform |
|---|---|---|
| running containers | `docker ps --format "{{ json . }}"` | `json_extract` |
| all containers | `docker ps -a --format "{{ json . }}"` | `json_extract` |
| images | `docker image ls --format "{{ json . }}"` | `json_extract` |
| volumes | `docker volume list --format "{{ json . }}"` | `json_extract` |
| networks | `docker network list --format "{{ json . }}"` | `json_extract` |
| contexts | `docker context list --format "{{ json . }}"` | `json_extract` |
| services, secrets, stacks, plugins, swarm nodes | (same pattern) | `json_extract` |

#### Kubectl — 13+ generators, NONE can be native

All require the Kubernetes API server. All use `-o custom-columns=:.metadata.name` or `-o name` — perfect for script + `split_lines`.

Generators include: resource types, resource names, pods, deployments, contexts, clusters, nodes, roles, cluster roles, cronjobs, containers within pods. All use `stale-while-revalidate` caching with 1-hour TTL in Fig.

#### SSH — 2 generators, BOTH can be native

| Generator | Fig's approach | Native? | Rationale |
|---|---|---|---|
| config hosts | `cat ~/.ssh/config` | **Yes** | Pure file parsing, `Host` directives, recursive `Include` |
| known hosts | `cat ~/.ssh/known_hosts` | **Yes** | Pure file parsing, line-per-host |

#### Make — 1 generator, CAN be native

Fig runs `make -qp | awk ...` but Makefile targets can be parsed directly with regex (`^target_name:`) — no binary needed.

#### npm/yarn — 4 generators, SOME can be native

| Generator | Native? | Rationale |
|---|---|---|
| scripts (package.json) | **Yes** | Parse `package.json` with serde_json, walk up directories |
| workspace names | **Yes** | Parse `package.json` `workspaces` field |
| installed packages | Script | `ls node_modules` or `npm list` |
| package search | Script (network) | Hits npms.io API |

#### Cargo — 9 generators, SOME can be native

| Generator | Native? | Rationale |
|---|---|---|
| packages (workspace) | **Yes** | Parse `Cargo.toml` with `toml` crate |
| features | **Yes** | Parse `Cargo.toml` `[features]` table |
| targets (bins/examples) | **Maybe** | Parse Cargo.toml + scan `src/bin/`, `examples/` |
| dependencies | **Maybe** | Parse Cargo.toml `[dependencies]` |
| compilation targets | Script | `rustc --print target-list` |
| crate search | Script (network) | Hits crates.io API |

#### Brew — 8 generators, NONE can be native

All require the brew binary. Script + `split_lines` or `regex_extract`.

#### systemctl, terraform, aws, gh, docker-compose — ALL script-only

All require their respective binaries talking to services/daemons.

**Summary classification:**

| Category | Count | Examples |
|---|---|---|
| **Already Rust-native** | 3 | `git_branches`, `git_tags`, `git_remotes` |
| **Can be native (pure file parsing)** | ~9 | SSH hosts, Make targets, npm scripts, Cargo packages/features, git stashes/aliases |
| **Must be script (needs running binary)** | ~70+ | Docker, Kubectl, Brew, systemctl, terraform, git status/diff/log, etc. |
| **Network/API (out of scope)** | ~5 | npm search, crate search, Docker Hub search |

**v0.2.0 decision:** Ship with the 3 existing Rust-native generators. Everything else uses the script + transform pipeline from Fig. The async pipeline infrastructure is needed regardless (for Docker, Kubectl, Brew, and 60+ other generators), so there is no value in blocking v0.2.0 to add more native generators.

**v0.2.x native promotion candidates** (post-launch, based on user feedback):
1. `ssh_config_hosts` + `ssh_known_hosts` — high value, trivial file parsing, SSH users are power users
2. `make_targets` — simple file parse, common tool
3. `npm_scripts` — trivial JSON parse, very common
4. `cargo_packages` + `cargo_features` — trivial TOML parse, natural fit for a Rust project

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
| `if (out.startsWith("X")) return []` | `[{"type": "error_guard", "starts_with": "X"}, ...]` |
| `JSON.parse(line)` with field access | `[..., {"type": "json_extract", "name": "$.field"}]` |
| `line.match(/regex/)` | `[..., {"type": "regex_extract", "pattern": "...", "name": 1}]` |
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
                { "type": "regex_extract", "pattern": "^(\\S+)\\s+(\\S+)", "name": 1, "description": 2 }
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
// Named transforms are plain strings: "split_lines", "trim", etc.
// Parameterized transforms are internally-tagged objects: { "type": "regex_extract", ... }
//
// The two shapes differ (string vs object), so custom Deserialize dispatches:
// - Strings → Named transforms via visit_str
// - Objects → Parameterized transforms via visit_map (internally-tagged, "type" field)
// Custom impl reports: "expected transform name (split_lines, trim, ...) or object with type field"
#[derive(Debug)]  // custom Deserialize impl, not #[derive]
enum Transform {
    Named(NamedTransform),
    Parameterized(ParameterizedTransform),
}

#[derive(Debug)]
enum NamedTransform {
    SplitLines,
    FilterEmpty,
    Trim,
    SkipFirst,
    Dedup,
}

// Internally-tagged: {"type": "regex_extract", "pattern": "...", "name": 1}
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ParameterizedTransform {
    #[serde(rename = "split_on")]   SplitOn { delimiter: String },
    #[serde(rename = "skip")]       Skip { n: usize },
    #[serde(rename = "take")]       Take { n: usize },
    #[serde(rename = "error_guard")]ErrorGuard { starts_with: Option<String>, contains: Option<String> },
    #[serde(rename = "regex_extract")]RegexExtract { pattern: String, name: usize, description: Option<usize> },
    #[serde(rename = "json_extract")]JsonExtract { name: String, description: Option<String> },
    #[serde(rename = "column_extract")]ColumnExtract { column: usize, description_column: Option<usize> },
}
```

**Pipeline phases:**

The transform pipeline operates in two phases to handle the type transition from raw output to structured lines:

1. **Pre-split phase** (input: `String`, output: `String`): `error_guard` runs here — if the raw output matches an error pattern, the pipeline short-circuits and returns no suggestions.
2. **Post-split phase** (input: `Vec<String>`, output: `Vec<Suggestion>`): All other transforms (`filter_empty`, `trim`, `regex_extract`, `json_extract`, etc.) operate on the split lines.

The `split_lines` / `split_on` transform is the boundary between phases. If no split transform is specified, the entire output is treated as a single-element list.

**Load-time validation (required):** The spec loader MUST validate transform ordering when specs are loaded. Invalid orderings produce a clear error at startup, not a confusing runtime failure:
- `error_guard` must appear before any split transform
- `split_lines` / `split_on` must appear at most once
- Post-split transforms (`filter_empty`, `trim`, `regex_extract`, etc.) must not appear before the split
- Violations: log a warning with spec name + transform list, skip the malformed generator (static completions still work)

**Execution model:**

1. `gc-suggest` encounters a generator with `script` + `transforms`
2. Execute shell command via `tokio::process::Command` with timeout (default 5s, configurable)
3. Capture stdout as `String`, discard stderr (logged at `tracing::debug` level)
4. Run pre-split transforms (error_guard)
5. Run split transform (split_lines / split_on)
6. Run post-split transforms (filter, trim, extract, etc.)
7. Output: `Vec<Suggestion>` ready for fuzzy ranking
8. If `cache` is configured, store results with TTL keyed by `(spec_name, resolved_command_argv, cwd)` — the resolved command includes post-substitution template values to prevent stale cache hits across different input contexts

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

**Dynamic result merge behavior:**
When dynamic results arrive after the popup is already showing static results:
1. **Append below static results.** Dynamic suggestions appear after all static suggestions. This preserves the user's navigation position — the selected item index does not change.
2. **Re-rank on next keystroke only.** Dynamic and static results are interleaved by fuzzy score only when the user types the next character. Until then, the two groups remain visually stable.
3. **Popup resize:** The popup grows downward (or upward if rendered above the cursor) to accommodate new items, capped at the configured max height. If already at max height, additional items are scrollable.
4. **Accepted before arrival:** If the user accepts a static suggestion before dynamic results arrive, the pending dynamic task is cancelled. No late merge occurs.
5. **Dismissed before arrival:** If the user dismisses the popup (Esc, cursor movement), pending dynamic tasks are cancelled.

**Safety constraints:**
- Commands are arrays (no shell expansion/injection): `["brew", "list", "-1"]` executes directly via `execvp`, NOT `sh -c`
- Timeout: default 5 seconds, configurable per-generator or globally via `config.toml`. On timeout: SIGTERM, wait 1s, SIGKILL.
- Concurrency: max 3 generator commands in flight concurrently (allows multi-generator specs without blocking the whole system). Per-spec semaphore prevents a single spec from hogging all slots.
- Working directory: current directory as tracked by OSC 7
- Environment: inherit from shell, but strip `GHOST_COMPLETE_ACTIVE` to prevent recursive invocation. stderr is discarded (not forwarded to terminal — would corrupt output).

**`script_template` substitution safety:**
When `script_template` substitutes `{prev_token}` or `{current_token}` into command arrays, the substitution always produces a single argv element. User input like `; rm -rf /` becomes the literal string `"; rm -rf /"` as one argument — not interpreted by a shell. However, the substituted value IS passed as an argument to external commands. Mitigations:
- The converter MUST NOT emit `script_template` for commands known to interpret arguments dangerously (e.g., commands with `--exec`, `eval`, or pattern-based interpretation)
- Substituted values are length-limited (1024 bytes) to prevent abuse via pathologically long input
- `tracing::warn` on any substitution containing shell metacharacters (`|`, `;`, `&`, `` ` ``, `$`) — not blocked, but visible in debug logs for spec auditing

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

**Cache key:** `(spec_name, resolved_command_argv, cwd_if_cache_by_directory)` — uses the fully resolved command (post-substitution for `script_template`) rather than a generator index, to prevent stale cache hits when the same generator runs with different `{prev_token}` or `{current_token}` values.
**Storage:** In-memory `HashMap` with expiry timestamps. No disk persistence.
**Invalidation:** TTL-based only. No filesystem watchers or event-based invalidation (YAGNI).

### Component 6: Spec Distribution

**Current (v0.1.x):** 34 specs embedded in the binary via `include_str!` and deployed to `~/.config/ghost-complete/specs/` during `ghost-complete install`. At runtime, specs are loaded from disk via `SpecStore::load_from_dir()`.

**New (v0.2.0): Single-source embedded approach.**

The v0.1.x download-based distribution (`update-specs` fetching tarballs from GitHub Releases) was rejected due to supply chain risk: downloaded specs contain `script` arrays — shell commands executed on the user's machine — with no practical way to verify integrity without a full signing infrastructure. Instead, all specs ship embedded in the binary and are deployed during `ghost-complete install`, exactly like v0.1.x but at scale.

**How it works:**

1. **All specs embedded via `include_str!`** — the converter produces JSON specs at build time. All ~700 specs are embedded in the binary, just like the existing 34. Binary size increase: ~10-15MB (acceptable for a local CLI tool).
2. **Single specs directory:** `~/.config/ghost-complete/specs/` — all specs deployed during `ghost-complete install`. No two-directory system, no precedence logic, no `update-specs` command.
3. **Specs update when the binary updates.** Users get new/improved specs by updating ghost-complete itself (via `cargo install`, Homebrew, or GitHub release). This matches user expectations for a CLI tool.
4. **Existing hand-written specs are replaced by converted specs.** The converter produces a single unified spec set. See "Rust-Native Generator Preservation" below for how fast-path generators are preserved.

**Hybrid generators — per-generator Rust-native / script selection:**

The decision between Rust-native and script-based execution is made **per generator within the same spec**, not per spec. This is the key insight: a single spec like `git.json` will contain a mix of both — Rust-native generators for completions we've implemented natively (branches, tags, remotes) and script-based generators for everything else from Fig (stash names, config keys, reflog entries, aliases, worktrees, etc.).

The converter applies this logic to each generator individually:

```
For each generator in a Fig spec:
  1. If generator.script matches NATIVE_GENERATOR_MAP → emit { "type": "..." }
  2. If generator has script + postProcess/splitOn     → emit { "script": [...], "transforms": [...] }
  3. If generator requires JS logic                     → emit { "requires_js": true }
```

**`NATIVE_GENERATOR_MAP` lookup table (v0.2.0):**

Only generators that exist today in `gc-suggest` are mapped. See the Dynamic Generator Audit in Research Findings for the full landscape and future promotion candidates.

```
NATIVE_GENERATOR_MAP = {
    ("git", "branch")     → { "type": "git_branches" },
    ("git", "tag")        → { "type": "git_tags" },
    ("git", "remote")     → { "type": "git_remotes" },
}
```

**Result:** The converted `git.json` contains *more* completions than either the hand-written spec or the raw Fig spec alone. Hand-written `git.json` had 3 Rust-native generators (branches, tags, remotes) and static subcommands/options. Fig's `git.ts` has ~14 generators. The hybrid output has 3 Rust-native generators (instant, synchronous) + ~11 script-based generators (async, from Fig — stashes, commits, staged files, aliases, etc.). Other tools like Docker (~18 generators), Kubectl (~13), Brew (~8) gain dynamic completions entirely through the script pipeline — they had zero dynamic completions in v0.1.x.

**Generator format coexistence in the engine:**

The engine supports both generator formats in the same spec:

- `{ "type": "git_branches" }` → existing Rust-native provider (instant, synchronous)
- `{ "type": "filepaths" }` → existing filesystem provider (instant, synchronous)
- `{ "script": [...], "transforms": [...] }` → shell-execution generator (async, transform pipeline)

The engine checks: if `type` is present, use the built-in Rust provider. If `script` is present, use the transform pipeline. A single spec routinely contains both (e.g., git uses native branch listing + script-based stash listing). Rust-native generators always take priority if both could serve the same completion context.

**Build process (CI, not user-facing):**
1. CI job runs the spec converter against `@withfig/autocomplete`
2. Converter produces JSON specs, applying the native generator map
3. Converted specs are committed to the repo (or generated as a build step) and embedded via `include_str!`
4. Users never need Node.js, npm, or network access for specs

## Spec Coverage Tiers at Launch

| Tier | Coverage | Specs | Approach |
|---|---|---|---|
| **Pure static** | Full subcommand/option/description tree, no dynamic generators | ~460 | Converter (zero JS, zero shell execution) |
| **Hybrid (Rust-native + script)** | Specs where some generators hit `NATIVE_GENERATOR_MAP`, rest are script-based | ~1 (git) | 3 native generators (branches, tags, remotes) + ~11 script generators from Fig |
| **Script-only dynamic** | Dynamic completions via script + transforms (no Rust-native match) | ~190 | Docker (~18 generators), Kubectl (~13), Brew (~8), SSH (~2), Make (~1), etc. |
| **JS-required (deferred)** | Marked but non-functional dynamic generators | ~85 | Future v0.3.0 (dynamic script + custom async) |
| **Total functional at launch** | | **~650 of 735** | **~89% coverage** |

All 735 specs ship as a single unified set. The hybrid per-generator approach means `git.json` gains ~11 dynamic completions (stashes, commits, staged files, aliases, etc.) on top of the 3 existing native ones. Tools like Docker, Kubectl, and Brew gain dynamic completions entirely — they had zero in v0.1.x. See the Dynamic Generator Audit for the full classification.

**Visibility for `requires_js` specs:** Specs with generators marked `requires_js: true` provide static completions (subcommands, options, descriptions) but no dynamic completions. To avoid user confusion:
- `tracing::info` when a generator is skipped due to `requires_js` (visible with `RUST_LOG=info`)
- `ghost-complete status` subcommand reports: N total specs, M fully functional, K partially functional (requires JS), with a list of affected commands
- This prevents users from thinking ghost-complete is broken when `aws` or `terraform` dynamic completions don't appear

## Performance Targets

| Operation | Target | Notes |
|---|---|---|
| Static completion (existing) | <50ms | No change |
| Transform pipeline execution | <1ms | Rust string processing |
| Shell command execution | <200ms | Async, with timeout |
| Cached generator hit | <1ms | HashMap lookup |
| Spec metadata loading (700 specs) | <50ms at startup | Eager metadata index (name → offset), lazy full parse |
| Full spec parse (on first use) | <5ms per spec | Parsed and cached on first completion trigger for that command |
| Memory for loaded specs | <15MB | Only actively-used specs fully parsed; benchmark to confirm |

## Testing Strategy

- **Transform pipeline unit tests:** Each transform function tested independently with known input/output
- **Transform validation tests:** Verify load-time ordering validation catches invalid pipelines (e.g., `trim` before `split_lines`)
- **Custom Deserialize tests:** Verify error messages from malformed transform JSON are actionable (not serde's generic "data did not match any variant")
- **Integration tests:** Full generator flow (mock shell command → transforms → suggestions)
- **Spec converter tests:** Known Fig TypeScript → expected JSON output for representative specs
- **Native generator map tests:** Verify converter emits `{ "type": "git_branches" }` (not `{ "script": ["git", "branch"] }`) for all commands in `NATIVE_GENERATOR_MAP`
- **Cache key tests:** Verify that `script_template` generators with different `{prev_token}` values produce different cache keys
- **Dynamic merge tests:** Verify popup behavior when dynamic results arrive (append, no index reset, cancellation on dismiss/accept)
- **`requires_js` visibility tests:** Verify `tracing::info` is emitted when a JS-required generator is skipped
- **Regression tests:** Existing 234 tests remain unchanged
- **Benchmark tests:** Transform pipeline latency on realistic command output sizes (100, 1000, 10000 lines)
- **Lazy loading benchmarks:** Confirm metadata-only loading stays under 50ms with 700 specs

## Migration & Compatibility

- **v0.1.x hand-written specs are replaced by hybrid conversions.** Running `ghost-complete install` on v0.2.0 deploys the new unified spec set, overwriting the old 34 specs. Each converted spec preserves Rust-native generators where they exist (per-generator, not per-spec) and adds script-based generators from Fig for everything else. Users see strictly more completions with no performance regression on existing fast paths.
- **No breaking config changes.** `config.toml` gains optional new fields (generator timeout, cache defaults) but existing configs work as-is.
- **Single spec directory.** The two-directory system and `update-specs` command from the original design are removed. All specs live in `~/.config/ghost-complete/specs/`, deployed by `ghost-complete install`.
- **`suggest_sync` API changes.** The `SuggestionEngine` gains a new async method `suggest_dynamic()` alongside the existing `suggest_sync()`. Callers (handler.rs) need updating to orchestrate both. This is an internal API change — no user-facing impact.
- **Rollback path:** If a converted spec produces worse completions than the v0.1.x hand-written version for a specific command, users can manually place a corrected JSON file in `~/.config/ghost-complete/specs/` — it will be overwritten on next `ghost-complete install`, but this provides a temporary escape hatch. Long-term fix: improve the converter.

## Future: Experimental JS Runtime (v0.3.0)

Reserved for a future version. Design notes for when we get there:

- **Runtime:** rquickjs (QuickJS via Rust FFI). ~4MB binary addition, <300μs context startup.
- **Feature flag:** `--features=js-runtime` at compile time, `experimental.js_runtime = true` in config at runtime.
- **Scope:** Execute `postProcess` JS functions and dynamic `script` functions from specs marked `requires_js: true`.
- **NOT in scope:** `custom` async generators (would require implementing Fig's `executeCommand` API — significant additional work).
- **Sandbox:** QuickJS context has no filesystem/network access. Only receives command stdout as input, returns suggestion array.

## Open Questions

1. **Spec converter implementation language:** Rust (parsing TypeScript AST with `tree-sitter`) vs Node.js script (can `require()` the compiled specs directly)? Node.js is simpler for the converter but adds a toolchain dependency to CI. tree-sitter has Rust bindings with a TypeScript grammar, which would keep the entire project in one toolchain. Decision deferred to converter implementation phase.
2. **Converter pattern coverage:** The converter's AST pattern matching will inevitably miss some `postProcess` patterns that ARE expressible as transforms but don't match the expected shape. Iteration on pattern coverage will happen post-launch as we discover edge cases. Specs that fail conversion gracefully degrade to `requires_js: true`.
3. **Native generator promotion timing:** The Dynamic Generator Audit identifies ~9 generators that *could* be Rust-native (SSH hosts, Make targets, npm scripts, Cargo packages/features, git stashes/aliases). These are deferred to v0.2.x — the script pipeline handles them fine for launch, and user feedback will determine which are worth the implementation effort.

## Summary of Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Spec format | Extend existing JSON with `script` + `transforms` | Backward-compatible, no new format to learn |
| Dynamic execution | Declarative transform pipeline in Rust | Covers ~89% of Fig specs, zero runtime overhead |
| JS runtime | Deferred to v0.3.0 under experimental flag | QuickJS (rquickjs), ~4MB, <300μs startup |
| Spec source | Convert from @withfig/autocomplete, replacing hand-written specs | Single source of truth; hybrid per-generator approach preserves Rust-native fast paths while adding script-based generators from Fig |
| Distribution | All specs embedded in binary via `include_str!` | No supply chain risk, no download mechanism, no integrity verification needed — specs update with binary |
| Caching | In-memory TTL-based, keyed by resolved command | Prevents stale hits from `script_template` substitution |
| Spec loading | Lazy: metadata eager, full parse on first use | 700 specs at startup would exceed latency/memory targets |
| Transform validation | Load-time ordering validation | Catches invalid pipelines at startup, not at runtime |
| Transform deserialization | Custom `Deserialize` impl (not `serde(untagged)`) | Actionable error messages for malformed specs |
| `requires_js` visibility | `tracing::info` + `ghost-complete status` subcommand | Prevents user confusion when dynamic completions are unavailable |
| Version | 0.2.0 | Major feature addition warranting minor version bump |
