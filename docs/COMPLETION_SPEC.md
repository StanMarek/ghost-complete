# Completion Spec Format

Ghost Complete uses a Fig-compatible JSON format for completion specs. Specs define the subcommands, options, and argument types for a CLI tool. Ghost Complete ships with 711 built-in specs covering common commands (git, docker, cargo, npm, kubectl, brew, and 700+ more).

## Overview

- Specs are JSON files, one per command
- Shipped specs are embedded in the binary at compile time
- Custom specs go in `~/.config/ghost-complete/specs/`
- The filename stem is the canonical command id (`git.json` is reachable as `git` on the shell). The spec's `name` field can differ from the stem and registers as a secondary alias when free — see [Command addressability](#command-addressability) below.
- Validate with `ghost-complete validate-specs`

## Schema

### CompletionSpec (root)

```json
{
  "name": "command-name",
  "description": "Short description of the command",
  "subcommands": [ ... ],
  "options": [ ... ],
  "args": [ ... ]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Command name (must match the binary name) |
| `description` | string | No | Short description shown in the popup |
| `subcommands` | SubcommandSpec[] | No | List of subcommands |
| `options` | OptionSpec[] | No | Top-level flags/options |
| `args` | ArgSpec[] | No | Positional argument definitions |

### SubcommandSpec

```json
{
  "name": "subcommand",
  "description": "What this subcommand does",
  "subcommands": [ ... ],
  "options": [ ... ],
  "args": [ ... ]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Subcommand name |
| `description` | string | No | Description shown in the popup |
| `priority` | u8 | No | Rank override, see [Priority and ranking](#priority-and-ranking) |
| `subcommands` | SubcommandSpec[] | No | Nested subcommands (recursive) |
| `options` | OptionSpec[] | No | Flags specific to this subcommand |
| `args` | ArgSpec[] | No | Positional argument definitions |

### OptionSpec

```json
{
  "name": ["--long-flag", "-s"],
  "description": "What this flag does",
  "args": { ... }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string[] | Yes | Flag names (long and/or short forms) |
| `description` | string | No | Description shown in the popup |
| `priority` | u8 | No | Rank override, see [Priority and ranking](#priority-and-ranking) |
| `args` | ArgSpec | No | If the flag takes a value, define it here |

### ArgSpec

```json
{
  "name": "argument-name",
  "description": "What this argument is",
  "template": "filepaths",
  "generators": [{ "type": "git_branches" }]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | No | Display name for the argument |
| `description` | string | No | Description of the argument |
| `template` | string | No | Built-in template: `"filepaths"` or `"folders"` |
| `generators` | GeneratorSpec[] | No | Dynamic generators for values |
| `suggestions` | SuggestionEntry \| SuggestionEntry[] | No | Static enum-like candidates, see below. Accepts either a single entry or an array; the array form is canonical |

#### Static suggestions

Specs may declare a fixed list of completion candidates for an arg position via the `suggestions` field. Each entry is either a plain string (shorthand) or a `SuggestionObject` with metadata.

```json
{
  "name": "format",
  "suggestions": [
    "tar",
    {
      "name": ["json", "j"],
      "description": "JSON output",
      "type": "arg",
      "priority": 80,
      "hidden": false
    }
  ]
}
```

| Field | Type | Required | Behavior |
|-------|------|----------|----------|
| `name` | `string \| string[]` | Yes | Each alias becomes its own ranked candidate |
| `description` | `string` | No | Forwarded to the candidate's description |
| `type` | `string` | No | Maps to `SuggestionKind` (see table below). Default: `EnumValue` |
| `priority` | `0..=100` | No | Per-item priority override |
| `hidden` | bool | No | When true, entry is dropped at load time |

**`type` mapping:**

| Fig `type` | `SuggestionKind` |
|------------|------------------|
| `"subcommand"` | `Subcommand` |
| `"option"` | `Flag` |
| `"file"` | `FilePath` |
| `"folder"` | `Directory` |
| `"arg"`, `"special"`, `"shortcut"`, `"mixin"`, `"auto-execute"`, missing | `EnumValue` |

Unknown `type` strings emit a load-time warning (visible via `ghost-complete validate-specs`) and fall back to `EnumValue`. Static suggestions surface in the candidate pool *unconditionally* — they are values for an arg slot, not commands, and are not affected by the suppression rules that hide subcommands and options when the user is filling in a flag's argument or has passed `--`.

Mirrors Fig's [`Suggestion`](https://fig.io/api/interfaces/Suggestion) interface (subset). Reserved fields (`insertValue`, `displayName`, `replaceValue`, `icon`, `isDangerous`) are silently ignored by the deserializer; v2 may surface them.

### GeneratorSpec

Generators produce dynamic suggestion candidates. Ghost Complete supports several generator types, described below.

#### Rust-native generators

Built-in generators that run natively without spawning external processes:

```json
{
  "type": "git_branches"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | Yes | Generator type identifier |

**Available native generator types:**

| Type | Produces |
|------|----------|
| `git_branches` | Local git branch names |
| `git_remotes` | Git remote names |
| `git_tags` | Git tag names |
| `git_files` | Tracked files in the git repo |
| `makefile_targets` | Targets parsed from the nearest ancestor `GNUmakefile`/`makefile`/`Makefile` (no `make` shellout). Filters meta targets, pattern rules, and variable-expanded targets. |
| `npm_scripts` | Keys of `scripts` in the nearest ancestor `package.json`; description is the script value (truncated to 120 chars). |
| `cargo_workspace_members` | Workspace member package names from the nearest ancestor `Cargo.toml`; falls back to the single `package.name` when no `[workspace]` table is present. |

The ux-14 migration expands provider-backed generators across cargo metadata targets/features, npm local dependency fields, Docker/Podman images/containers/volumes/networks, kubectl contexts/namespaces/resources/pods/nodes/services, tmux sessions/windows/panes/clients, systemd units, Homebrew formulae/casks, and macOS `dscl` principals. See [`docs/PROVIDERS.md`](./PROVIDERS.md) for the full provider table and counter contracts.

AWS SDK-backed generators are available through the opt-in `aws_sdk` provider; see [`docs/PROVIDERS.md`](./PROVIDERS.md) for configuration, credentials, fallback behavior, and status counters.

#### Script generators

Execute an external command and turn its stdout into suggestions. The command is executed directly (no shell expansion via `sh -c`).

```json
{
  "script": ["brew", "list", "-1"],
  "transforms": ["split_lines", "filter_empty", "trim"],
  "cache": { "ttl_seconds": 300, "cache_by_directory": false }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `script` | string[] | Yes | Command and arguments, executed without shell expansion |
| `transforms` | string[] | No | Transform pipeline applied to stdout (see [Transforms](#transforms)) |
| `cache` | CacheConfig | No | TTL caching configuration (see [Cache](#cache)) |
| `_lowered_from_requires_js` | boolean | No | Internal converter marker for generators lowered from JS to native `script` + `transforms`; counted by `status --json` as `requires_js_generators_lowered_to_transforms` |
| `_static_extracted_subprocess` | boolean | No | Internal converter marker for skipped Fig subprocess wrappers lifted to native `script` + `transforms`; counted by `status --json` as `requires_js_generators_static_extracted` |

#### Script template generators

Like script generators, but with token interpolation. `{current_token}` is replaced with the user's current input before execution.

```json
{
  "script_template": ["docker", "inspect", "{current_token}"],
  "transforms": ["split_lines"]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `script_template` | string[] | Yes | Command with `{current_token}` placeholders |
| `transforms` | string[] | No | Transform pipeline applied to stdout |
| `cache` | CacheConfig | No | TTL caching configuration |
| `_lowered_from_requires_js` | boolean | No | Internal converter marker for generators lowered from JS to native `script_template` + `transforms`; counted by `status --json` as `requires_js_generators_lowered_to_transforms` |
| `_static_extracted_subprocess` | boolean | No | Internal converter marker for skipped Fig subprocess wrappers lifted to native `script_template` + `transforms`; counted by `status --json` as `requires_js_generators_static_extracted` |

#### JS-backed generators (`requires_js`)

Some Fig specs contain generators that require JavaScript execution. All `js_runtime.kind` variants execute via [`gc-jsrt`](../crates/gc-jsrt/) — a bounded QuickJS evaluator running on a dedicated worker thread. See [`docs/JS_RUNTIME.md`](./JS_RUNTIME.md) for the runtime model (sandbox, host API, resource caps, kill switch).

```json
{
  "requires_js": true,
  "script": ["echo", "hello\nworld"],
  "js_runtime": {
    "kind": "post_process",
    "source": "out => out.split('\\n').filter(Boolean).map(name => ({ name }))"
  }
}
```

| `js_runtime.kind` | Behaviour | Status |
|-------------------|-----------|--------|
| `post_process`    | Run `script` (or `script_template`) as a normal script generator, then pass stdout through the JS function in `js_runtime.source`. The function returns the suggestion list. | Active. |
| `script_function` | Evaluate `js_runtime.source` to produce an `argv`, spawn that argv, then parse stdout with the generator transforms or default line splitting. | Active. |
| `custom`          | No script — `js_runtime.source` is an async function that returns suggestions directly (the Fig `custom: async () => [...]` shape). | Active. |
| `token_only`      | Evaluate `js_runtime.source` with only `tokens`, `currentToken`, and `previousToken` installed. It returns suggestions directly and cannot access `fig`, `__ghost`, cwd/env, or `executeShellCommand`. | Active. |

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `kind` | string | Yes | One of `post_process`, `script_function`, `custom`, `token_only` (see table above). |
| `source` | string | Yes | The JS function source or expression. For `post_process` it receives stdout and returns suggestions; for `custom` it returns suggestions directly; for `script_function` its evaluation yields the argv to spawn; for `token_only` it may be a direct expression or a function receiving `(tokens, ctx)`. |
| `timeout_ms` | integer | No | Per-generator override of the global JS execution timeout. |
| `allow_shell_command` | boolean | No | Default `false`. Currently effective only for `custom` generators that call the host `executeShellCommand` binding with a shell string. `script_function` generators return argv for the engine to spawn and are not given a shell runner. Required only for explicitly-audited shipped specs. |
| `self_contained` | boolean | No | Default `false`. Required for `script_function` and `custom` dispatch; the converter sets it only after proving the source has no unresolved helper/module bindings. Not required for `token_only`, because that runtime exposes no host API. |

Example `token_only` generator:

```json
{
  "requires_js": true,
  "js_runtime": {
    "kind": "token_only",
    "self_contained": false,
    "source": "(tokens, ctx) => ctx.previousToken === 'get' ? ['pods', 'services'].map(name => ({ name })) : []"
  }
}
```

Token-only runtime generators run inside `JsRuntimeKind::TokenOnly` with `tokens`, `searchTerm`, and `currentToken` exposed but no `executeShellCommand`, cwd, env, `fig`, or `__ghost` host API. They provide sync-like latency for pure token logic and do not loosen the `self_contained` requirement for `script_function` or `custom` generators.

The converter populates `js_runtime` for `post_process` bodies the matcher cannot lower to declarative transforms. For Fig `script: (...) => [...]` and `custom: async () => [...]` sources, it emits `script_function` / `custom` only when static analysis proves the function is self-contained; closure-dependent bodies that are host-API-free are promoted to `token_only`, and the rest remain `requires_js` without runtime metadata and are skipped. The runtime can be disabled wholesale via `[suggest.providers] js_runtime = false` in `config.toml`; in that mode static portions (subcommands, options) of `requires_js` specs continue to work and the JS-backed generators silently no-op.

When a Fig JS post-processor is lowered all the way to native transforms, the resulting generator must not keep `requires_js`. The converter may retain `_lowered_from_requires_js: true` as private metadata so `ghost-complete status --json` can report migration progress through the top-level `requires_js_generators_lowered_to_transforms` field and `counters.lowered_to_transforms`.

When a skipped Fig `custom` or function-valued `script` is only a single literal subprocess call plus stdout post-processing, the converter may lift it into native `script`/`script_template` + `transforms`. Those generators must not keep `requires_js` or `js_runtime`; the converter retains `_static_extracted_subprocess: true` so `ghost-complete status --json` can report migration progress through `requires_js_generators_static_extracted` and `counters.static_extracted_subprocess`.

#### Available Templates

| Template | Behavior |
|----------|----------|
| `filepaths` | Complete with files and directories |
| `folders` | Complete with directories only |

### Transforms

Transforms process the raw stdout of a script generator into individual suggestion strings. They are specified as an ordered array in the `transforms` field and are applied left-to-right.

**Simple transforms:**

| Transform | Effect |
|-----------|--------|
| `split_lines` | Split stdout on newlines into individual suggestions |
| `filter_empty` | Remove empty strings |
| `trim` | Trim whitespace from each suggestion |
| `skip_first` | Skip the first line (e.g., header rows) |
| `dedup` | Remove duplicate suggestions |

**Parameterized transforms:**

| Transform | Effect |
|-----------|--------|
| `split_on(delim)` | Split on a custom delimiter instead of newlines |
| `skip(n)` | Skip the first N lines |
| `take(n)` | Keep only the first N lines |
| `regex_extract(pattern, name_group, desc_group?)` | Extract suggestion name (and optional description) via regex capture groups |
| `json_extract(name_field, desc_field?)` | Parse each line as JSON and extract fields |
| `json_extract_array(path, item_name?, item_description?, split_on?, split_index?)` | Terminal transform: parse the entire output as JSON and emit one suggestion per element of the array at `path`. Cannot follow a split transform; only `suffix` may follow it. |
| `json_path_extract(array, name_field?, description_field?, priority_field?)` | Terminal transform: parse the entire output as one JSON document, resolve `array`, and project optional fields from each element. `array` supports one `[*]` wildcard segment. Cannot follow a split transform; only `suffix` may follow it. |
| `column_extract(column, desc_column?)` | Extract whitespace-separated columns by position (0-indexed) |
| `error_guard(starts_with?, contains?)` | Return empty results if stdout matches an error pattern. Both fields are optional and may be supplied together — a match against either fires the guard. |
| `suffix(value)` | Append a fixed literal to each suggestion's text |

`json_path_extract` accepts the same internally tagged object style as the other parameterized transforms:

```json
{
  "script": ["aws", "iam", "list-roles"],
  "transforms": [
    {
      "type": "json_path_extract",
      "array": "Roles",
      "name_field": "RoleName",
      "description_field": "Arn"
    }
  ],
  "_lowered_from_requires_js": true
}
```

The deserializer also accepts the external tag spelling documented by the ux-10b plan:

```json
{
  "json_path_extract": {
    "array": "Roles[*]",
    "name_field": "RoleName"
  }
}
```

**Ordering rules:** Transforms are validated at spec load time. A splitting transform (`split_lines` or `split_on`) must appear before any per-line transforms like `trim`, `filter_empty`, or `dedup`. Placing a per-line transform before a splitter is a validation error. `json_extract_array` and `json_path_extract` are terminal — they must not appear after a splitter, and only `suffix` may follow them.

### Cache

Script generators can cache their results to avoid re-executing slow commands on every keystroke. Cache is configured per-generator via the `cache` field.

```json
{
  "script": ["kubectl", "get", "pods", "-o", "name"],
  "transforms": ["split_lines", "trim"],
  "cache": { "ttl_seconds": 60, "cache_by_directory": true }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `ttl_seconds` | integer | none | How long (in seconds) to cache results before re-executing |
| `cache_by_directory` | boolean | `false` | Key the cache by the current working directory. Enable this for script commands whose output depends on CWD (for example, a project-local tool invocation) |

When `cache_by_directory` is `false`, the cache key is the generator's script command alone. When `true`, the CWD is appended to the key so that switching directories produces fresh results.

## Examples

### Minimal spec (cd)

```json
{
  "name": "cd",
  "description": "Change directory",
  "args": [
    { "name": "directory", "template": "folders" }
  ]
}
```

### Spec with subcommands and options (cargo)

```json
{
  "name": "cargo",
  "description": "Rust package manager",
  "subcommands": [
    {
      "name": "build",
      "description": "Compile the current package",
      "options": [
        { "name": ["--release"], "description": "Build with optimizations" },
        { "name": ["-p", "--package"], "description": "Package to build" }
      ]
    },
    {
      "name": "test",
      "description": "Run the tests",
      "options": [
        { "name": ["-p", "--package"], "description": "Package to test" },
        { "name": ["--no-fail-fast"], "description": "Run all tests regardless of failure" }
      ]
    }
  ]
}
```

### Spec with option arguments and generators

```json
{
  "name": "git",
  "description": "Distributed version control system",
  "subcommands": [
    {
      "name": "checkout",
      "description": "Switch branches or restore files",
      "args": [
        {
          "name": "branch",
          "generators": [{ "type": "git_branches" }]
        }
      ],
      "options": [
        {
          "name": ["-b"],
          "description": "Create and switch to a new branch",
          "args": { "name": "new-branch" }
        }
      ]
    }
  ]
}
```

## Priority and ranking

Suggestions sort by `(non-history-first, score-desc, priority-desc, alpha)`.
Score is the fuzzy match score against the current word. Priority is an
integer in `0..=100`; higher ranks earlier. Ties break alphabetically.

History suggestions are partitioned to the bottom regardless of priority
— frecency-boosted history can still outrank other history, but never
outranks non-history.

When a `SubcommandSpec` or `OptionSpec` sets `priority` explicitly, that
value is used. Otherwise the engine falls back to a per-kind base:

| Kind          | Base | Notes                                          |
| ------------- | ---- | ---------------------------------------------- |
| GitBranch     | 80   |                                                |
| GitTag        | 75   |                                                |
| GitRemote     | 70   |                                                |
| Subcommand    | 70   |                                                |
| ProviderValue | 70   | Generator output (npm, docker, kubectl, …)     |
| EnumValue     | 65   | Static `args.suggestions` enum-like values        |
| EnvVar        | 50   |                                                |
| Command       | 40   | `$PATH` binaries                               |
| Flag          | 30   |                                                |
| Directory     | 25   |                                                |
| FilePath      | 20   |                                                |
| History       | 10   |                                                |

Override per-item only when the default ordering is wrong for a specific
completion (for example, `git push origin main` setting `priority: 100`
on the default branch). Mirrors Fig's
[`Suggestion.priority`](https://fig.io/api/interfaces/Suggestion#priority)
field, so upstream Fig-converter specs that already set it are honoured.

## Bundled spec priorities

A subset of the 711 specs shipped in `specs/` carry curated `priority`
values on the most-used subcommands and on dangerous flags — most
specs rely entirely on the per-kind base values from the table above.
Three layers stack to produce any explicit `priority` field:

1. Upstream Fig priorities — preserved by the `tools/fig-converter/`
   pipeline. Re-running the converter against a new
   `@withfig/autocomplete` release picks up Fig's hand-tuning.
2. Heuristic bumps — applied by `tools/spec-priority-audit/apply.mjs`
   from a curated `heuristics.json` ruleset (11 command families:
   vcs, package_manager, container, kubernetes, cloud, build_tool,
   ssh_remote, shell_builtin, file_modifier, http_tools, editor).
   `tools/spec-priority-audit/heuristics.json` is the canonical
   source — keep this list in sync with it. Never overwrites
   existing values.
3. Manual edits — case-by-case overrides in `specs/*.json` directly.

Re-running the audit script after editing `heuristics.json` is safe;
it only writes where `priority` is missing.

## Suppression contract

The popup runs providers in one of six contexts, classified per
keystroke from `(current_word, in_redirect, word_index, spec_matched)`.
Precedence is top-to-bottom — the first row that matches wins.

| Context           | Triggered when                                    | Providers that run                  |
| ----------------- | ------------------------------------------------- | ----------------------------------- |
| `CommandPosition` | `word_index == 0`                                 | `$PATH` commands + history          |
| `Redirect`        | after `>`, `>>`, `<`, `<<`, `<<<`                 | filesystem only                     |
| `PathPrefix`      | current_word starts with `./`, `/`, `~/`, `../`   | filesystem + history — escape hatch |
| `FlagPrefix`      | current_word starts with `-` or `--`              | spec flags + spec subcommands only  |
| `SpecArg`         | spec matched + arg position resolved              | only what the spec declares         |
| `UnspeccedArg`    | no spec at all                                    | filesystem + history (fallback)     |

**Filesystem only enters the candidate set when the matched arg position
explicitly declares `template: "filepaths"` or `template: "folders"`,
OR when the user typed a path-prefix.**

**Generators only run at arg positions where they are declared.** No
spec field is required to opt out — the absence of a generator is the
opt-out.

## Command addressability

The shell command users type maps to a parsed spec via the loader's
**alias index**. There are two layers:

1. **Filename stem (canonical id).** `git.json` is always reachable as
   `git`. The stem is the stable identifier used by status reporting
   (`commands_addressable`, `iter()` keys) and by `ghost-complete
   doctor`'s diagnostics. Filename stems are case-sensitive on the
   shell, so `R.json` and `r.json` are distinct addressable commands.

2. **`CompletionSpec.name` (secondary alias).** When the spec's `name`
   field differs from the stem, the loader registers `name` as an
   additional alias — provided that key isn't already taken. For
   example, `kubecolor.json` declares `name: "kubectl"`, so it
   normally would register both `kubecolor` and `kubectl` as keys —
   but the canonical `kubectl.json` already owns `kubectl` (its stem
   takes precedence over another file's name claim), so the
   `kubecolor → kubectl` alias is rejected and the `kubecolor` spec
   stays reachable only as `kubecolor`.

**Conflict surfacing.** When two files want the same alias, the loader
records an `AliasConflict` and the rejected file keeps every other
addressability path it had. Conflicts come in three flavours:

- `DuplicateName` — two files declare the same `CompletionSpec.name`
  and neither stem matches it (e.g., `tns.json` and `nativescript.json`
  both declare `name: "ns"`).
- `NameMatchesOtherStem` — one spec's `name` collides with another
  file's filename stem (e.g., `kubecolor.json` declares `name:
  "kubectl"` and `kubectl.json` exists).
- `DirectoryPrecedence` — same filename in two configured spec dirs.
  Earlier dirs in `paths.spec_dirs` win, which is how user overrides
  shadow the embedded fallback (`~/.config/ghost-complete/specs/git.json`
  beats the embedded `git.json`).

Run `ghost-complete status --json` to see the current
`commands_addressable` count and `command_alias_conflicts` total.
`ghost-complete doctor` lists per-conflict diagnostics with kind-specific
hints, and `status --json` (schema 1.2) emits the structured list under
`command_alias_conflict_details`. The conflicts are also available on
`SpecStore::conflicts()` for in-process consumers.

## Validation

Validate your specs with:

```bash
ghost-complete validate-specs
```

This checks JSON syntax and schema compliance, reporting errors with file names and line numbers.

## How Spec Resolution Works

When you type a command, Ghost Complete:

1. Looks up the spec by the command name (first word)
2. Walks through your typed arguments, matching subcommands greedily
3. At the deepest matched position, collects available subcommands, options, and generators
4. If the preceding word was a flag that takes an argument, uses that flag's `args` spec for templates/generators
5. Feeds all candidates into the fuzzy ranker with your current input as the query
6. Script generators execute asynchronously — the popup appears immediately with any static candidates (subcommands, options, templates) and script results merge into the popup progressively as they complete
