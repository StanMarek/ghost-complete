# Configuration Reference

Ghost Complete reads its configuration from `~/.config/ghost-complete/config.toml`. All fields are optional — unset values use their defaults.

Run `ghost-complete install` to generate a default config with all fields documented as comments.

## Sections

### `[trigger]`

Controls when the autocomplete popup appears.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `auto_chars` | char[] | `[' ', '/', '-', '.']` | Characters that trigger suggestion after typing |
| `delay_ms` | integer | `150` | Milliseconds to wait after typing pauses before showing suggestions. Set to `0` to disable debounce (trigger immediately). |
| `auto_trigger` | boolean | `true` | When `false`, disables all automatic popup triggers. Only the manual keybinding opens the popup. |

```toml
[trigger]
auto_chars = [' ', '/', '-', '.']
delay_ms = 150
auto_trigger = true
```

### `[popup]`

Controls the popup appearance.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_visible` | integer | `10` | Maximum number of suggestions shown at once |
| `borders` | bool | `false` | Draw a border around the popup |
| `feedback_dismiss_ms` | integer | `1200` | Milliseconds to keep Empty/Error feedback visible. Set to `0` to disable auto-dismiss. Values above `10000` are clamped. |
| `spinner` | bool | `true` | Animate Loading feedback when the popup is wide enough |
| `show_provider_errors` | bool | `false` | Show provider names in error feedback. Disabled by default for shared-screen privacy. |

```toml
[popup]
max_visible = 10
borders = false
feedback_dismiss_ms = 1200
spinner = true
show_provider_errors = false
```

Popup width is calculated automatically from suggestion content, clamped between 20 and 60 columns.

### `[suggest]`

Controls the suggestion engine behavior.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_results` | integer | `50` | Maximum total candidates to consider |
| `max_history_results` | integer | `5` | Maximum history entries shown in popup. Set to `0` to disable history. |
| `generator_timeout_ms` | integer | `5000` | Per-invocation timeout (milliseconds) for script and git generators. |

```toml
[suggest]
max_results = 50
max_history_results = 5
generator_timeout_ms = 5000
```

Shell history loads up to 10,000 entries.

### `[suggest.providers]`

Enable or disable individual suggestion providers.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `commands` | bool | `true` | `$PATH` command completions |
| `filesystem` | bool | `true` | File and directory completions |
| `specs` | bool | `true` | Fig-compatible JSON spec completions |
| `git` | bool | `true` | Git context completions (branches, tags, remotes) |

```toml
[suggest.providers]
commands = true
filesystem = true
specs = true
git = true
```

### `[paths]`

Override default file paths.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `spec_dirs` | string[] | `[]` | Directories to load completion specs from. Supports `~` expansion. When empty, the loader auto-detects in this order: `~/.config/ghost-complete/specs/`, then `<exe-dir>/specs`, then `./specs`, then the embedded specs in the binary. When set, the configured directories are used instead — but if all of them are missing or unreadable, the loader falls back to the same auto-detection chain. |

```toml
[paths]
spec_dirs = ["~/.config/ghost-complete/specs", "/usr/local/share/ghost-complete/specs"]
```

### `[keybindings]`

Customize keyboard shortcuts. Each value is a key name string. Invalid key names cause a startup error (fail-fast).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `accept` | string | `"tab"` | Accept the selected suggestion |
| `accept_and_enter` | string | `"enter"` | Accept and execute |
| `dismiss` | string | `"escape"` | Dismiss the popup |
| `navigate_up` | string | `"arrow_up"` | Move selection up |
| `navigate_down` | string | `"arrow_down"` | Move selection down |
| `trigger` | string | `"ctrl+/"` | Manually trigger completions |

```toml
[keybindings]
accept = "tab"
accept_and_enter = "enter"
dismiss = "escape"
navigate_up = "arrow_up"
navigate_down = "arrow_down"
trigger = "ctrl+/"
```

#### Key Name Syntax

- Lowercase letters: `a` through `z`
- Special keys: `tab`, `enter`, `escape`, `backspace`, `space`
- Arrow keys: `arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`
- Modifiers: `ctrl+<key>` (e.g., `ctrl+space`, `ctrl+/`)

### `[theme]`

Customize popup colors and styles. Values are space-separated SGR token strings. Invalid styles cause a startup error (fail-fast). Changes are applied live when config hot-reload is active.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `preset` | string | `""` | Base preset: `dark`, `light`, `catppuccin`, `material-darker`. Empty uses `dark`. Field overrides below take priority over preset values. |
| `selected` | string | (from preset) | Style for the selected (highlighted) item |
| `description` | string | (from preset) | Style for suggestion descriptions |
| `match_highlight` | string | (from preset) | Style for fuzzy-matched characters |
| `item_text` | string | (from preset) | Style for non-selected item text |
| `scrollbar` | string | (from preset) | Style for the scrollbar track |
| `border` | string | (from preset) | Style for the popup border |
| `feedback_loading` | string | (from preset) | Style for Loading feedback |
| `feedback_empty` | string | (from preset) | Style for Empty feedback |
| `feedback_error` | string | (from preset) | Style for provider Error feedback |

```toml
[theme]
preset = "catppuccin"
# Override individual fields from the preset:
match_highlight = "underline"
feedback_error = "dim fg:#d20f39"
```

#### Presets

| Preset | Selected | Description | Match Highlight | Item Text | Scrollbar | Border | Feedback Error |
|--------|----------|-------------|-----------------|-----------|-----------|--------|----------------|
| `dark` | `reverse` | `dim` | `bold` | *(none)* | `dim` | `dim` | `dim fg:#f38ba8` |
| `light` | `fg:#1e1e2e bg:#dce0e8 bold` | `fg:#6c6f85` | `fg:#d20f39 bold` | *(none)* | `fg:#9ca0b0` | `fg:#9ca0b0` | `dim fg:#d20f39` |
| `catppuccin` | `fg:#cdd6f4 bg:#585b70 bold` | `fg:#6c7086` | `fg:#f9e2af bold` | *(none)* | `fg:#585b70` | `fg:#585b70` | `dim fg:#f38ba8` |
| `material-darker` | `fg:#eeffff bg:#424242 bold` | `fg:#616161` | `fg:#ffcb6b bold` | *(none)* | `fg:#424242` | `fg:#424242` | `dim fg:#ff5370` |

All presets leave `item_text` unstyled (default terminal foreground). `feedback_loading` and `feedback_empty` inherit `description` by default. Override `item_text` to colorize non-selected items.

#### Style String Syntax

Styles are space-separated tokens:

| Token | Effect |
|-------|--------|
| `bold` | Bold text |
| `dim` | Dim/faint text |
| `underline` | Underlined text |
| `reverse` | Swap foreground/background |
| `fg:N` | Set foreground to 256-color index N (0-255) |
| `bg:N` | Set background to 256-color index N (0-255) |
| `fg:#RRGGBB` | Set foreground to 24-bit truecolor |
| `bg:#RRGGBB` | Set background to 24-bit truecolor |

Examples:
- `"reverse"` — inverted colors (default selected style)
- `"bold fg:255"` — bold white text
- `"dim"` — faint text (default description style)
- `"fg:#cdd6f4 bg:#585b70 bold"` — Catppuccin-style selection
- `"bold underline fg:208"` — bold underlined orange text

### `[experimental]`

Opt-in features that are not yet considered stable.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `multi_terminal` | bool | `false` | Enable unsupported/unknown terminals. All 9 supported terminals (Ghostty, Kitty, WezTerm, Alacritty, Rio, iTerm2, Terminal.app, Zed, VSCode) work without this flag. Set to `true` only if you want to try Ghost Complete on an unlisted terminal. |

```toml
[experimental]
multi_terminal = true
```

Ghost Complete auto-detects the terminal via `TERM_PROGRAM` and terminal-specific env vars (`KITTY_WINDOW_ID`, `WEZTERM_UNIX_SOCKET`, `ALACRITTY_SOCKET`, `ZED_TERM`, `VSCODE_IPC_HOOK_CLI`), then selects the appropriate rendering strategy:

- **Ghostty, Kitty, WezTerm, Rio, Zed** — DECSET 2026 synchronized output, native OSC 133 prompt markers.
- **VSCode** (and forks: VSCodium, Cursor, Windsurf, Positron, Trae) — DECSET 2026 synchronized output via xterm.js, native OSC 133. Coexists with VSCode's own shell integration: the proxy forwards the editor's OSC 633 sequences untouched so command decorations / sticky scroll / "run recent command" keep working, and Ghost Complete's own shell integration suppresses its redundant OSC 7771 emission when `VSCODE_INJECTION=1` is set.
- **Alacritty** — DECSET 2026 synchronized output, OSC 7771 shell integration prompt markers (Alacritty does not support OSC 133).
- **iTerm2 / Terminal.app** — pre-render buffer (single `write()` atomicity), OSC 7771 shell integration prompt markers.

**tmux support:** Ghostty, Kitty, WezTerm, Alacritty, iTerm2, Zed, and VSCode are detected inside tmux via their respective env vars. Terminal.app inside tmux is not detected (it sets no env var that leaks through tmux).

## Logging

Ghost Complete logs through the `tracing` crate. Logging is configured via CLI flags and the `RUST_LOG` environment variable, not via `config.toml`.

### CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--log-level <level>` | `warn` | One of `trace`, `debug`, `info`, `warn`, `error`. Ignored when `RUST_LOG` is set. |
| `--log-file <path>` | (see below) | Write logs to this file. When unset in proxy mode, the default path is used. |

### Default Log File

In proxy mode (`ghost-complete` wrapping the shell), logs default to a file — never stderr — to avoid corrupting the terminal stream. The default path is:

```
$XDG_STATE_HOME/ghost-complete/ghost-complete.log
```

When `XDG_STATE_HOME` is unset, it falls back to:

```
~/.local/state/ghost-complete/ghost-complete.log
```

The parent directory is created automatically on startup. If directory creation fails, Ghost Complete prints a one-line warning to stderr and falls back to stderr logging for the duration of that run.

Subcommands (`status`, `doctor`, `validate-specs`, `config`, `install`, `uninstall`) log to stderr by default; pass `--log-file` to redirect them.

### Level Hierarchy

```
error < warn < info < debug < trace
```

Setting `--log-level info` enables `info`, `warn`, and `error` events. `trace` is the most verbose and includes every internal decision point.

### `RUST_LOG` Precedence

`RUST_LOG` is read first and overrides `--log-level` when both are set. It uses the standard [`tracing-subscriber` `EnvFilter` syntax](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html), which supports per-crate, per-module, and per-span filters.

Examples:

```bash
# Everything at debug or higher
RUST_LOG=debug ghost-complete

# Debug only in the suggest engine; everything else at warn
RUST_LOG=warn,gc_suggest=debug ghost-complete

# Debug in the suggest engine and info in the PTY loop
RUST_LOG=gc_suggest=debug,gc_pty=info ghost-complete

# Trace a single module
RUST_LOG=gc_parser::osc=trace ghost-complete
```

Crate names use underscores (e.g. `gc_suggest`), not hyphens. Filter directives are comma-separated; the first bare level (if any) sets the global default.

### Tail-f Recipe

Open the log in a second terminal while reproducing an issue:

```bash
tail -f "${XDG_STATE_HOME:-$HOME/.local/state}/ghost-complete/ghost-complete.log"
```

### Generating a Bug Report

1. Start the proxy with verbose logging: `ghost-complete --log-level debug`.
2. Reproduce the bug in that session.
3. Attach `$XDG_STATE_HOME/ghost-complete/ghost-complete.log` (or the fallback path) to the GitHub issue.

For crate-targeted investigations, combine `--log-level` with `RUST_LOG`, e.g. `RUST_LOG=gc_pty=trace ghost-complete` to inspect the PTY loop in isolation.

## Full Example

```toml
[trigger]
auto_chars = [' ', '/', '-']
delay_ms = 200

[popup]
max_visible = 8

[suggest]
max_results = 100
max_history_results = 3

[suggest.providers]
commands = true
filesystem = true
specs = true
git = false

[paths]
spec_dirs = ["~/.config/ghost-complete/specs"]

[keybindings]
accept = "tab"
accept_and_enter = "enter"
dismiss = "escape"
trigger = "ctrl+/"

[theme]
preset = "catppuccin"
match_highlight = "underline"
```

## Notes

- **Config hot-reload:** Some fields are applied live without restarting your shell. Others require a shell restart. See the table below.
- **Nerd Font icons:** The popup gutter uses Nerd Font icons. If your terminal font doesn't include Nerd Font patches, you'll see placeholder characters. Use a [Nerd Font](https://www.nerdfonts.com/) for the best experience.
- **History control:** Use `max_history_results` (not `providers.history`) to control history. Set to `0` to disable history entirely.
- **Popup navigation:** PageUp, PageDown, Home, and End navigate the popup when it is visible and are forwarded to the shell when it is hidden. These structural keys are not user-configurable.

### Hot-Reload Behavior

| Section | Fields | Live Reload |
|---------|--------|:-----------:|
| `[theme]` | All fields | Yes |
| `[keybindings]` | All fields | Yes |
| `[trigger]` | `auto_chars` | Yes |
| `[trigger]` | `delay_ms` | No |
| `[trigger]` | `auto_trigger` | Yes |
| `[popup]` | `max_visible`, `borders`, `feedback_dismiss_ms`, `spinner`, `show_provider_errors` | Yes |
| `[suggest]` | All fields | No |
| `[suggest.providers]` | All fields | No |
| `[paths]` | All fields | No |
| `[experimental]` | All fields | No |

Fields marked "No" require a shell restart (`source ~/.zshrc` or open a new terminal).
