# Architecture

Ghost Complete is a terminal-native autocomplete engine that works as a PTY proxy — it sits between your terminal emulator and your shell, intercepting the data stream to render suggestion popups using native ANSI escape sequences. No Accessibility API, no IME hacks, no Electron overlay.

## How It Works

```
┌──────────────────────────────────────────────────────────┐
│                  Terminal Emulator                        │
│       (Ghostty, Kitty, WezTerm, Alacritty, ...)         │
│       Receives: shell output + overlay sequences         │
└──────────────────────┬───────────────────────────────────┘
                       │ stdin / stdout (raw bytes)
                       ▼
              ┌─────────────────┐
              │  Ghost Complete  │
              │  (PTY Proxy)     │
              │                  │
              │  ┌────────────┐  │
              │  │ VT Parser  │◄─┼── parses shell output, tracks cursor
              │  └─────┬──────┘  │   position, screen dims, prompt bounds
              │        │         │
              │        ▼         │
              │  ┌────────────┐  │
              │  │ Buffer     │  │
              │  │ Tracker    │──┼── reconstructs current command line,
              │  └─────┬──────┘  │   detects command context
              │        │         │
              │        ▼         │
              │  ┌────────────┐  │
              │  │ Suggestion │  │
              │  │ Engine     │──┼── fuzzy matching against completions
              │  └─────┬──────┘  │   (specs, filesystem, git, history)
              │        │         │
              │        ▼         │
              │  ┌────────────┐  │
              │  │ Overlay    │  │
              │  │ Renderer   │──┼── renders popup using ANSI sequences
              │  └────────────┘  │   with synchronized output
              │                  │
              └────────┬─────────┘
                       │ PTY master ↔ slave
                       ▼
              ┌──────────────────┐
              │  Shell Process   │
              │  (zsh/bash/fish) │
              └──────────────────┘
```

## Data Flow

1. User types a keystroke in the terminal emulator
2. `gc-pty` receives it on stdin — if the popup is visible, intercept navigation keys (Tab, arrows, Escape, Enter); otherwise forward to the shell PTY
3. Shell produces output, which flows through `gc-parser` (VT state tracking) then to terminal stdout
4. `gc-parser` tracks cursor position, screen dimensions, and prompt boundaries using OSC 133 / OSC 7771 markers
5. On trigger conditions (space after command, `/`, `-`, `--`, Ctrl+/, or delay timeout), `gc-suggest` computes ranked suggestions
6. Static suggestions (subcommands, options, templates) render immediately via `gc-overlay`
7. Script generators execute async in background; results merge into the popup progressively without resetting cursor position

## Crate Map

The workspace contains 8 crates under `crates/`:

| Crate | Purpose | Key Dependencies |
|-------|---------|------------------|
| [`ghost-complete`](../crates/ghost-complete/) | Binary entry point, CLI (`clap`), install/uninstall, `status`, `doctor`, `validate-specs` | clap |
| [`gc-pty`](../crates/gc-pty/) | PTY proxy event loop — spawns shell, multiplexes stdin/stdout with `tokio::select!`, handles SIGWINCH, async generator merge | portable-pty, tokio |
| [`gc-parser`](../crates/gc-parser/) | VT escape sequence parsing — cursor position, screen dimensions, prompt boundaries (OSC 133 + OSC 7771), CWD (OSC 7) | vte |
| [`gc-buffer`](../crates/gc-buffer/) | Command line reconstruction — current command, argument position, pipes, redirects, quotes | |
| [`gc-suggest`](../crates/gc-suggest/) | Suggestion engine — dispatches to providers, fuzzy-ranks with nucleo, async generators with transform pipelines and TTL caching | nucleo, serde_json |
| [`gc-overlay`](../crates/gc-overlay/) | ANSI popup rendering — cursor save/restore, synchronized output, scroll-to-make-room, scrollbar, fuzzy match highlighting | |
| [`gc-config`](../crates/gc-config/) | TOML config, keybindings, themes (presets + custom styles), generator timeouts | serde, toml |
| [`gc-terminal`](../crates/gc-terminal/) | Terminal detection and capability profiling — `TerminalProfile` with `RenderStrategy` and `PromptDetection` enums | |

### Dependency Graph

```
ghost-complete ──► gc-pty ──► gc-parser
                     │    ──► gc-buffer
                     │    ──► gc-suggest
                     │    ──► gc-overlay
                     │    ──► gc-config
                     │    ──► gc-terminal
                     │
                     ├── gc-parser (VT parsing)
                     ├── gc-buffer (command context)
                     ├── gc-suggest (suggestions)
                     ├── gc-overlay (rendering)
                     ├── gc-config (configuration)
                     └── gc-terminal (terminal detection)
```

`gc-overlay`, `gc-parser`, `gc-suggest`, `gc-buffer`, and `gc-config` are independent of each other — only `gc-pty` ties them together.

## Key Design Decisions

### PTY Proxy over Shell Plugin

Ghost Complete runs as a PTY proxy rather than a zsh/fish plugin. The proxy sits between the terminal and the shell, seeing all bytes in both directions. This means:

- **No zle widget conflicts** — doesn't hook into shell internals
- **No plugin manager dependencies** — one binary, works after install
- **No RPROMPT corruption** — popup rendering is independent of shell prompt
- **Shell-agnostic core** — the same proxy works with zsh, bash, and fish

The tradeoff is complexity: we have to maintain our own VT parser to track cursor position, rather than asking the shell where it is.

### Parser-Only VT Tracking (vte)

We use the `vte` crate — a parser-only VT state machine that fires callbacks per escape sequence. We do NOT maintain a full screen buffer (like `alacritty_terminal` or `vt100`). We only track:

- Cursor position (row, column)
- Screen dimensions
- Prompt boundaries (via OSC 133 / OSC 7771)
- Current working directory (via OSC 7)

This keeps memory usage minimal and parsing fast. The tradeoff: cursor position can drift from reality over time (complex escape sequences we don't fully model). We correct for this using CPR sync — periodically requesting the terminal's actual cursor position via `CSI 6n` and reconciling.

### Nucleo for Fuzzy Matching

`nucleo` (the fuzzy matcher from Helix editor) is ~6x faster than `skim`. It presegments Unicode strings once and reuses them across queries, making incremental search fast. With 10,000 candidates, nucleo returns results in <1ms. For an autocomplete tool running on every keystroke, this is the difference between "instant" and "noticeable lag."

### Synchronized Output (DECSET 2026)

Modern terminals support DECSET 2026 — the terminal buffers all output between begin/end markers and renders it atomically. This eliminates flicker during popup rendering. Ghostty, Kitty, WezTerm, Alacritty, and Rio all support this.

For terminals that don't (iTerm2, Terminal.app), we fall back to a pre-render buffer strategy: build the entire frame into a byte buffer and emit it in a single `write()` syscall, relying on kernel write atomicity.

### Terminal Capability Profiling

The `gc-terminal` crate detects the terminal at startup and assigns capabilities via a `TerminalProfile`:

- **RenderStrategy** — `Synchronized` (DECSET 2026) or `PreRenderBuffer` (single write)
- **PromptDetection** — `Osc133` (native) or `ShellIntegration` (OSC 7771 markers)

Detection uses `TERM_PROGRAM` plus terminal-specific env vars (`KITTY_WINDOW_ID`, `WEZTERM_UNIX_SOCKET`, `ALACRITTY_SOCKET`). Inside tmux, these env vars leak through from the outer terminal, allowing detection of the host terminal.

The overlay and parser crates are strategy-driven — they query the profile for capabilities rather than checking terminal names. Adding a new terminal means adding one enum variant and one match arm in `gc-terminal`; no other crate needs changes.

## Proxy Task Architecture

The PTY proxy runs three concurrent tokio tasks:

| Task | Role | Channel |
|------|------|---------|
| **Task A** (stdin reader) | Reads user keystrokes, intercepts popup navigation when visible, forwards to shell PTY | stdin → PTY master |
| **Task B** (PTY reader) | Reads shell output, runs VT parser, detects triggers, renders popup, forwards to stdout | PTY master → stdout |
| **Task D** (debounce timer) | Waits for typing pauses, fires delayed suggestion triggers | `tokio::sync::Notify` |

Task B notifies Task D via `Notify` when the buffer is dirty but no immediate trigger fired. Task D resets its timer on each notification and fires a trigger after `delay_ms` (default 150ms) of inactivity.

## Completion Spec Architecture

Ghost Complete ships 709 Fig-compatible JSON specs embedded in the binary via `include_str!`. At startup, specs are deserialized and indexed by command name.

Specs support multiple generator types:

| Type | Execution | Latency |
|------|-----------|---------|
| **Rust-native** (`git_branches`, `git_tags`, etc.) | Sync, in-process | Instant |
| **Templates** (`filepaths`, `folders`) | Sync, filesystem | Instant |
| **Script generators** (shell commands) | Async, spawned process | Variable (cached with TTL) |
| **Script templates** (commands with `{current_token}`) | Async, spawned process | Variable |

Script generator output passes through a transform pipeline (`split_lines`, `trim`, `regex_extract`, `json_extract`, `column_extract`, etc.) that is validated at spec load time.

Generator results are cached in-memory with configurable TTL per-generator. `cache_by_directory` keys cache entries by CWD for commands whose output is directory-dependent.

## Popup Rendering

The popup is rendered entirely via ANSI escape sequences — no alternate screen buffer, no TUI framework. The rendering flow:

1. Calculate viewport deficit (does the popup fit below the cursor?)
2. If not, scroll the viewport by emitting newlines at the bottom
3. Save cursor (DECSC)
4. For each visible suggestion: position cursor (CUP), apply styling (SGR), write text
5. Restore cursor (DECRC)

All of this is wrapped in DECSET 2026 begin/end markers (or pre-rendered into a single buffer for terminals without synchronized output).

**Scrollback protection**: The popup area is cleared by overwriting with spaces, never by using ED (Erase Display) or EL (Erase Line) — those would push popup text into scrollback history.

## Performance

| Metric | Target | Achieved |
|--------|--------|----------|
| Keystroke to suggestion | <50ms | <20ms typical |
| PTY forwarding overhead | <1ms | <1ms |
| Fuzzy match (10k candidates) | <1ms | <1ms (nucleo) |
| Memory (idle) | <10MB | ~8MB |
| Startup | <100ms | <50ms |

Benchmarks use Criterion and live in `gc-suggest` and `gc-parser`. Run with `cargo bench`.

## Shell Integration

Shell integration scripts in `shell/` emit semantic prompt markers:

- **OSC 133** — standard semantic prompt protocol (supported by Ghostty, Kitty, WezTerm, Rio)
- **OSC 7771** — Ghost Complete's own marker (used as fallback on Alacritty, iTerm2, Terminal.app)

Both are emitted simultaneously by the integration scripts, so the parser can use whichever the terminal supports.

Without shell integration, features are limited — prompt boundary detection falls back to heuristics, and manual trigger (Ctrl+/) is the only way to invoke completions.
