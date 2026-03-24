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

```toml
[trigger]
auto_chars = [' ', '/', '-', '.']
delay_ms = 150
```

### `[popup]`

Controls the popup appearance and size.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_visible` | integer | `10` | Maximum number of suggestions shown at once |
| `min_width` | integer | `20` | Minimum popup width in columns |
| `max_width` | integer | `60` | Maximum popup width in columns |

```toml
[popup]
max_visible = 10
min_width = 20
max_width = 60
```

### `[suggest]`

Controls the suggestion engine behavior.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_results` | integer | `50` | Maximum total candidates to consider |
| `max_history_results` | integer | `5` | Maximum history entries shown in popup. Set to `0` to disable history. |
| `max_history_entries` | integer | `10000` | Maximum shell history entries to load from `$HISTFILE` |
| `generator_timeout_ms` | integer | `5000` | Timeout in milliseconds for shell command generators. Commands that exceed this are killed. |

```toml
[suggest]
max_results = 50
max_history_results = 5
max_history_entries = 10000
generator_timeout_ms = 5000
```

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
| `spec_dirs` | string[] | `[]` | Additional directories to load completion specs from. When set, replaces the default `~/.config/ghost-complete/specs/`. Supports `~` expansion. |

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

```toml
[theme]
preset = "catppuccin"
# Override individual fields from the preset:
match_highlight = "underline"
```

#### Presets

| Preset | Selected | Description | Match Highlight | Scrollbar |
|--------|----------|-------------|-----------------|-----------|
| `dark` | `reverse` | `dim` | `bold` | `dim` |
| `light` | `fg:#1e1e2e bg:#dce0e8 bold` | `fg:#6c6f85` | `fg:#d20f39 bold` | `fg:#9ca0b0` |
| `catppuccin` | `fg:#cdd6f4 bg:#585b70 bold` | `fg:#6c7086` | `fg:#f9e2af bold` | `fg:#585b70` |
| `material-darker` | `fg:#eeffff bg:#424242 bold` | `fg:#616161` | `fg:#ffcb6b bold` | `fg:#424242` |

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
| `multi_terminal` | bool | `false` | Enable iTerm2 and Terminal.app support. When `false` (default), Ghost Complete only runs on Ghostty — on other terminals it transparently falls back to a plain shell. |

```toml
[experimental]
multi_terminal = true
```

When enabled, Ghost Complete auto-detects the terminal via `TERM_PROGRAM` and selects the appropriate rendering strategy:

- **Ghostty** — DECSET 2026 synchronized output, native OSC 133 prompt markers
- **iTerm2 / Terminal.app** — pre-render buffer (single `write()` atomicity), OSC 7771 shell integration prompt markers

**Known limitation:** Terminal.app inside tmux is not detected (Terminal.app sets no env var that leaks through tmux). Ghostty and iTerm2 in tmux work correctly.

## Full Example

```toml
[trigger]
auto_chars = [' ', '/', '-']
delay_ms = 200

[popup]
max_visible = 8
min_width = 25
max_width = 50

[suggest]
max_results = 100
max_history_results = 3
max_history_entries = 5000
generator_timeout_ms = 5000

[suggest.providers]
commands = true
filesystem = true
specs = true
git = false

[keybindings]
accept = "tab"
accept_and_enter = "enter"
dismiss = "escape"
trigger = "ctrl+/"

[theme]
preset = "catppuccin"
match_highlight = "underline"

[experimental]
multi_terminal = false
```

## Notes

- **Config hot-reload:** Changes to `config.toml` are applied live without restarting your shell. Theme, keybindings, trigger chars, and popup dimensions are all reloaded automatically.
- **Nerd Font icons:** The popup gutter uses Nerd Font icons. If your terminal font doesn't include Nerd Font patches, you'll see placeholder characters. Use a [Nerd Font](https://www.nerdfonts.com/) for the best experience.
- **History control:** Use `max_history_results` (not `providers.history`) to control history. Set to `0` to disable history entirely.
