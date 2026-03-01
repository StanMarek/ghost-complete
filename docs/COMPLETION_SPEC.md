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

```json
{
  "type": "git_branches"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | Yes | Generator type identifier |

#### Available Generator Types

| Type | Produces |
|------|----------|
| `git_branches` | Local git branch names |
| `git_remotes` | Git remote names |
| `git_tags` | Git tag names |
| `git_files` | Tracked files in the git repo |

#### Available Templates

| Template | Behavior |
|----------|----------|
| `filepaths` | Complete with files and directories |
| `folders` | Complete with directories only |

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
