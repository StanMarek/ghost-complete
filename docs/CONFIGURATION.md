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
| `max_history_entries` | integer | `10000` | Maximum shell history entries to load |

```toml
[suggest]
max_results = 50
max_history_entries = 10000
```

### `[suggest.providers]`

Enable or disable individual suggestion providers.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `commands` | bool | `true` | `$PATH` command completions |
| `history` | bool | `true` | Shell history completions |
| `filesystem` | bool | `true` | File and directory completions |
| `specs` | bool | `true` | Fig-compatible JSON spec completions |
| `git` | bool | `true` | Git context completions (branches, tags, remotes) |

```toml
[suggest.providers]
commands = true
history = true
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
| `trigger` | string | `"ctrl+space"` | Manually trigger completions |

```toml
[keybindings]
accept = "tab"
accept_and_enter = "enter"
dismiss = "escape"
navigate_up = "arrow_up"
navigate_down = "arrow_down"
trigger = "ctrl+space"
```

#### Key Name Syntax

- Lowercase letters: `a` through `z`
- Special keys: `tab`, `enter`, `escape`, `backspace`, `space`
- Arrow keys: `arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`
- Modifiers: `ctrl+<key>` (e.g., `ctrl+space`, `ctrl+n`)

### `[theme]`

Customize popup colors and styles. Values are space-separated SGR token strings. Invalid styles cause a startup error (fail-fast).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `selected` | string | `"reverse"` | Style for the selected (highlighted) item |
| `description` | string | `"dim"` | Style for suggestion descriptions |

```toml
[theme]
selected = "reverse"
description = "dim"
```

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

Examples:
- `"reverse"` — inverted colors (default selected style)
- `"bold fg:255"` — bold white text
- `"dim"` — faint text (default description style)
- `"fg:255 bg:236"` — white text on dark gray background
- `"bold underline fg:208"` — bold underlined orange text

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
max_history_entries = 5000

[suggest.providers]
commands = true
history = true
filesystem = true
specs = true
git = false

[keybindings]
accept = "tab"
accept_and_enter = "enter"
dismiss = "escape"
trigger = "ctrl+space"

[theme]
selected = "fg:255 bg:236"
description = "dim"
```
