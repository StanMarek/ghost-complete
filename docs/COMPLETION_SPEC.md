# Completion Spec Format

Ghost Complete uses a Fig-compatible JSON format for completion specs. Specs define the subcommands, options, and argument types for a CLI tool.

## File Location

Specs are loaded from `~/.config/ghost-complete/specs/`. The file name should match the command name (e.g., `git.json` for the `git` command).

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

#### Deferred JS generators

Some Fig specs contain generators that require JavaScript execution. Ghost Complete does not implement a JS runtime. When a generator is flagged as requiring JS, the static portions of the spec (subcommands, options, simple args) still work normally — only the JS-dependent generator is skipped.

```json
{
  "requires_js": true,
  "script": ["fallback", "command"]
}
```

If a `script` field is present alongside `requires_js`, the script is executed as a regular script generator (without JS evaluation).

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
| `column_extract(column, desc_column?)` | Extract whitespace-separated columns by position (0-indexed) |
| `error_guard(starts_with\|contains)` | Return empty results if stdout matches the given error pattern |

**Ordering rules:** Transforms are validated at spec load time. A splitting transform (`split_lines` or `split_on`) must appear before any per-line transforms like `trim`, `filter_empty`, or `dedup`. Placing a per-line transform before a splitter is a validation error.

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
| `cache_by_directory` | boolean | `false` | Key the cache by the current working directory. Enable this for commands whose output depends on CWD (e.g., `kubectl` with context, `make` targets) |

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
