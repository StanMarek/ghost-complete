# Ghost Complete — Implementation Plan

> A terminal-native autocomplete engine using PTY proxying, built in Rust.
> Personal tool, built for Ghostty. Other terminals may work but are not a priority.

## Problem Statement

Fig (now dead, acqui-hired into AWS) was the best terminal autocomplete experience available — but it only worked properly in iTerm2. It relied on macOS Accessibility API and IME hacks to position floating popups, which broke spectacularly in Ghostty and other modern terminals.

After switching from iTerm2 to Ghostty, that autocomplete experience disappeared. Amazon Q Developer CLI (Fig's successor) still uses the same broken approach: treating the terminal as a black box and overlaying native OS windows on top.

**Ghost Complete** solves this by sitting *inside* the terminal's data stream — a PTY proxy that intercepts I/O between the terminal emulator and the shell, renders popups using native ANSI sequences, and never needs to query the terminal for cursor coordinates because it already knows them.

### Scope

This is a personal project, built to scratch an itch. The primary (and currently only) target is **Ghostty**, which supports the full feature set needed: synchronized output (DECSET 2026), OSC 133 semantic prompts, OSC 7 CWD reporting, Kitty keyboard protocol, and 24-bit color. No graceful degradation, no fallback heuristics, no compatibility shims — just the happy path.

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│           Terminal Emulator (Ghostty)                    │
│           Receives: shell output + overlay sequences     │
└──────────────────────┬───────────────────────────────────┘
                       │ stdin / stdout (raw bytes)
                       ▼
              ┌─────────────────┐
              │  Ghost Complete │
              │  (PTY Proxy)    │
              │                 │
              │  ┌───────────┐  │
              │  │ VT Parser │◄─┼──── parses shell output, tracks cursor
              │  └─────┬─────┘  │     position, screen dimensions, prompt
              │        │        │     boundaries
              │        ▼        │
              │  ┌───────────┐  │
              │  │ Buffer    │  │
              │  │ Tracker   │──┼──── knows current command, argument
              │  └─────┬─────┘  │     position, working directory
              │        │        │
              │        ▼        │
              │  ┌───────────┐  │
              │  │ Suggestion│  │
              │  │ Engine    │──┼──── fuzzy matching against completions
              │  └─────┬─────┘  │     (filesystem, git, history, specs)
              │        │        │
              │        ▼        │
              │  ┌───────────┐  │
              │  │ Overlay   │  │
              │  │ Renderer  │──┼──── renders popup using ANSI sequences
              │  └───────────┘  │     with synchronized output (DECSET 2026)
              │                 │
              └────────┬────────┘
                       │ PTY master ↔ slave
                       ▼
              ┌─────────────────┐
              │  Shell Process  │
              │  (zsh/bash/fish)│
              └─────────────────┘
```

### Data Flow

1. User types a keystroke in the terminal emulator
2. Ghost Complete receives it on stdin
3. If popup is visible, intercept navigation keys (↑/↓/Tab/Esc/Enter)
4. Otherwise, forward keystroke to the shell PTY
5. Shell produces output → Ghost Complete parses it with VT parser
6. VT parser updates internal terminal state (cursor pos, screen buffer)
7. Shell output is forwarded to terminal emulator's stdout
8. If trigger condition met (e.g., after space, `/`, `-`), compute suggestions
9. Render popup overlay below cursor using ANSI sequences wrapped in synchronized output

---

## Tech Stack

### Core Dependencies

| Crate | Version | Purpose | Why This One |
|-------|---------|---------|--------------|
| `portable-pty` | 0.9+ | PTY creation and management | Battle-tested in WezTerm; macOS-focused for now; trait-based design allows runtime implementation swapping |
| `vte` | 0.13+ | ANSI/VT escape sequence parser | Production-proven in Alacritty; implements Paul Williams' state machine; parser-only (lightweight); handles all standard + xterm sequences |
| `crossterm` | 0.28+ | Terminal manipulation (cursor, colors, styling) | Only modern cross-platform option; `queue!` macro batches operations before flush (reduces syscalls); handles raw mode, alternate screen |
| `nucleo` | 0.5+ | Fuzzy matching | ~6x faster than skim; uses Smith-Waterman with presegmented Unicode; same scoring algorithm as fzf (familiar ranking); <1ms on 10k candidates |
| `tokio` | 1.x | Async runtime | Ecosystem integration (signal handling, process spawning, fs); `select!` macro ideal for multiplexing PTY I/O; well-documented for PTY proxy patterns |
| `serde` + `toml` | 1.x / 0.8+ | Configuration | Industry standard; human-readable config files; derive macros minimize boilerplate |
| `tracing` | 0.1+ | Structured logging | Async-aware; span-based context; essential for debugging PTY proxy timing |
| `clap` | 4.x | CLI argument parsing | Derive-based API; shell completion generation; subcommands for daemon/client modes |

### Secondary Dependencies

| Crate | Purpose |
|-------|---------|
| `nix` | Low-level POSIX operations (signal forwarding, `SIGWINCH` handling) |
| `dirs` | Platform-specific config/cache directory resolution |
| `notify` | File system watching (for config hot-reload) |
| `serde_json` | Completion spec parsing (Fig-compatible JSON format) |
| `shell-words` | Shell argument splitting/quoting |

### Why These Choices

**portable-pty over nix/rustix/raw libc:**
`portable-pty` provides `PtyPair` (master + slave handles), `CommandBuilder` for spawning shells, and automatic `SIGWINCH` forwarding. With raw `nix`, you'd write ~200 lines of unsafe PTY setup code that `portable-pty` handles in 5 lines. It's the same library WezTerm uses for its PTY layer.

**vte over vt100/alacritty_terminal/termwiz:**
`vte` is parser-only — it fires callbacks for each parsed sequence but doesn't maintain screen state. This is ideal because we only need to track *cursor position* and *prompt boundaries*, not a full screen buffer. `vt100` crate maintains a full grid which is unnecessary overhead. `alacritty_terminal` is not published to crates.io and is tightly coupled to Alacritty internals.

**tokio over smol/async-std:**
While `smol` has ~8µs lower base latency, `tokio` provides `tokio::process::Command` for PTY child management, `tokio::signal` for `SIGWINCH`/`SIGTERM` handling, and `tokio::select!` for clean I/O multiplexing. The latency difference is negligible compared to the 15-30ms total keystroke-to-suggestion target.

**nucleo over skim/fuzzy-matcher:**
Nucleo presegments Unicode strings once and reuses them across queries, making incremental search (as the user types) dramatically faster. With 10,000 completion candidates, nucleo returns results in <1ms while skim takes 5-10ms. For an autocomplete tool, this difference is the line between "instant" and "noticeable lag."

---

## Project Structure

```
ghost-complete/
├── Cargo.toml                  # Workspace manifest
├── Cargo.lock
├── README.md
├── LICENSE
├── docs/
│   ├── IMPLEMENTATION_PLAN.md  # This file
│   └── COMPLETION_SPEC.md      # Completion spec format docs
│
├── crates/
│   ├── ghost-complete/         # Main binary crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs         # Entry point, CLI parsing, daemon launch
│   │
│   ├── gc-pty/                 # PTY proxy layer
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── proxy.rs        # Core PTY proxy event loop
│   │       ├── spawn.rs        # Shell spawning and PTY pair creation
│   │       └── resize.rs       # SIGWINCH handling and PTY resize
│   │
│   ├── gc-parser/              # Terminal state tracking
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── state.rs        # Cursor position, screen dimensions
│   │       ├── performer.rs    # vte::Perform implementation
│   │       └── prompt.rs       # Prompt boundary detection (OSC 133)
│   │
│   ├── gc-buffer/              # Command buffer tracking
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── tracker.rs      # Current command line buffer
│   │       ├── context.rs      # Command context (which cmd, which arg)
│   │       └── parser.rs       # Shell syntax parsing (pipes, redirects)
│   │
│   ├── gc-suggest/             # Suggestion engine
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── engine.rs       # Dispatcher: routes to correct provider
│   │       ├── filesystem.rs   # Path completions (async with tokio::fs)
│   │       ├── git.rs          # Branch names, remotes, tags
│   │       ├── history.rs      # Shell history search
│   │       ├── commands.rs     # Command completions from $PATH
│   │       ├── specs.rs        # Fig-compatible completion spec loader
│   │       └── fuzzy.rs        # Nucleo integration, scoring, ranking
│   │
│   ├── gc-overlay/             # Popup rendering
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── renderer.rs     # ANSI sequence generation for popup
│   │       ├── layout.rs       # Popup positioning (above/below cursor)
│   │       ├── style.rs        # Colors, borders, highlights
│   │       └── cleanup.rs      # Popup dismissal and area restoration
│   │
│   └── gc-config/              # Configuration
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── config.rs       # TOML config struct with serde
│           ├── keybindings.rs  # Keybinding definitions
│           └── theme.rs        # Color theme definitions
│
├── shell/                      # Shell integration scripts
│   ├── ghost-complete.zsh      # Zsh integration (precmd/preexec hooks)
│   ├── ghost-complete.bash     # Bash integration (PROMPT_COMMAND)
│   └── ghost-complete.fish     # Fish integration (event handlers)
│
├── specs/                      # Built-in completion specs
│   ├── git.json
│   ├── docker.json
│   ├── cargo.json
│   ├── npm.json
│   └── kubectl.json
│
└── tests/
    ├── integration/            # End-to-end PTY proxy tests
    └── fixtures/               # Recorded terminal sessions for replay
```

---

## Implementation Phases

### Phase 1: PTY Proxy Foundation (Weeks 1–2)

**Goal:** A transparent PTY proxy that spawns a shell, forwards all I/O, and doesn't break anything.

**Deliverables:**
- `gc-pty` crate: spawn shell in PTY, forward stdin→shell and shell→stdout
- Raw mode handling: put terminal into raw mode, restore on exit
- Signal forwarding: `SIGWINCH` → PTY resize, `SIGTERM`/`SIGINT` → clean shutdown
- Zero-overhead passthrough: user shouldn't notice the proxy exists

**Key implementation details:**

```rust
// Simplified event loop (gc-pty/src/proxy.rs)
async fn run_proxy(shell: &str) -> Result<()> {
    let pty_pair = portable_pty::native_pty_system()
        .openpty(PtySize { rows: 24, cols: 80, .. })?;

    let child = pty_pair.slave.spawn_command(
        CommandBuilder::new(shell)
    )?;

    let mut reader = pty_pair.master.try_clone_reader()?;
    let mut writer = pty_pair.master.take_writer()?;

    // Enter raw mode
    crossterm::terminal::enable_raw_mode()?;

    tokio::select! {
        // stdin → shell
        result = forward_stdin_to_pty(&mut writer) => { ... }
        // shell → stdout
        result = forward_pty_to_stdout(&mut reader) => { ... }
        // SIGWINCH
        _ = signal::unix::signal(SignalKind::window_change())? => {
            let size = crossterm::terminal::size()?;
            pty_pair.master.resize(PtySize {
                rows: size.1, cols: size.0, ..
            })?;
        }
    }

    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}
```

**Validation:** Run `ghost-complete -- zsh`, use the shell normally. Vim, htop, ssh sessions should all work perfectly. Latency should be imperceptible.

---

### Phase 2: Terminal State Tracking (Weeks 3–4)

**Goal:** Parse all shell output to track cursor position, screen dimensions, and prompt boundaries.

**Deliverables:**
- `gc-parser` crate: implement `vte::Perform` to track cursor position
- Prompt detection via OSC 133 (semantic prompt sequences) — Ghostty supports this natively
- Current working directory tracking via OSC 7 — Ghostty supports this natively
- Screen dimension tracking (from SIGWINCH + initial query)

**Key implementation details:**

```rust
// gc-parser/src/performer.rs
struct TerminalState {
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
    prompt_row: Option<u16>,     // Where the prompt starts
    cwd: Option<PathBuf>,        // From OSC 7
    in_prompt: bool,             // Between prompt start and command execution
}

impl vte::Perform for TerminalState {
    fn print(&mut self, c: char) {
        self.cursor_col += 1;
        if self.cursor_col >= self.screen_cols {
            self.cursor_col = 0;
            self.cursor_row += 1;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0A => { self.cursor_row += 1; }  // LF
            0x0D => { self.cursor_col = 0; }   // CR
            0x08 => { self.cursor_col = self.cursor_col.saturating_sub(1); } // BS
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8],
                     _ignore: bool, action: char) {
        match action {
            'H' | 'f' => { /* CUP: set cursor position */ }
            'A' => { /* CUU: cursor up */ }
            'B' => { /* CUD: cursor down */ }
            'C' => { /* CUF: cursor forward */ }
            'D' => { /* CUB: cursor back */ }
            'J' => { /* ED: erase display */ }
            'K' => { /* EL: erase line */ }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        // OSC 7: current working directory
        // OSC 133: semantic prompt sequences (FinalTerm)
    }
}
```

**Validation:** Log cursor position continuously. Compare against actual cursor position (query with `CSI 6 n` device status report). They should match within ±1 cell.

---

### Phase 3: Command Buffer Tracking (Weeks 5–6)

**Goal:** Know exactly what the user is typing, which command they're in, and which argument position they're at.

**Deliverables:**
- `gc-buffer` crate: reconstruct current command line from terminal state
- Shell integration scripts that mark prompt boundaries (precmd/preexec hooks)
- Command context detection: `git checkout <branch>` → knows we need branch names
- Pipe and redirect awareness: `cat file.txt | grep <pattern>` → knows we're in grep's args

**Shell integration approach:**

```zsh
# shell/ghost-complete.zsh
# Inject OSC 133 semantic prompt markers
_gc_precmd() {
    # Mark: prompt is about to be displayed
    printf '\e]133;A\a'
}

_gc_preexec() {
    # Mark: command is about to execute
    printf '\e]133;C\a'
}

# Report current buffer on every keystroke (for real-time suggestions)
_gc_line_changed() {
    # Send buffer content via escape sequence to the PTY proxy
    printf '\e]133;D;%s\a' "$BUFFER"
}

precmd_functions+=(_gc_precmd)
preexec_functions+=(_gc_preexec)
zle -N self-insert _gc_self_insert_wrapper
```

**Note:** Since this is a personal Ghostty + zsh setup, the zsh integration is the primary path. Bash and fish scripts exist for completeness but are not actively tested.

---

### Phase 4: Suggestion Engine (Weeks 7–9)

**Goal:** Fast, relevant suggestions based on command context.

**Deliverables:**
- `gc-suggest` crate with pluggable suggestion providers
- Filesystem provider: async directory listing with `tokio::fs`
- Git provider: branch names, tags, remotes, modified files
- History provider: search shell history file (~/.zsh_history)
- Command provider: executables in $PATH
- Spec provider: load Fig-compatible completion specs (JSON)
- Nucleo-based fuzzy ranking across all providers

**Completion spec format (Fig-compatible):**

```json
{
  "name": "git",
  "subcommands": [
    {
      "name": "checkout",
      "args": [
        {
          "name": "branch",
          "generators": [
            { "type": "git_branches" }
          ]
        }
      ],
      "options": [
        { "name": ["-b", "--branch"], "description": "Create and checkout new branch" }
      ]
    }
  ]
}
```

**Performance targets:**
- Filesystem listing (1000 entries): < 5ms
- Git branch listing: < 10ms
- Fuzzy match on 10k candidates: < 1ms (nucleo)
- Total suggestion latency: < 20ms

---

### Phase 5: Popup Overlay Rendering (Weeks 10–12)

**Goal:** Render a floating popup below (or above) the cursor without corrupting scrollback.

**Deliverables:**
- `gc-overlay` crate: ANSI-based popup rendering
- Synchronized output wrapping (DECSET 2026) for flicker-free updates
- Intelligent positioning: below cursor if space, above if near bottom
- Popup cleanup: restore terminal area on dismissal
- Scrollback protection: never push popup content into scrollback

**Rendering approach:**

```rust
// gc-overlay/src/renderer.rs
fn render_popup(
    stdout: &mut impl Write,
    suggestions: &[Suggestion],
    selected: usize,
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
) -> Result<()> {
    let popup_height = suggestions.len().min(MAX_VISIBLE) as u16;
    let popup_width = suggestions.iter()
        .map(|s| s.display_text.len())
        .max().unwrap_or(20)
        .min(60) as u16 + 4; // padding + border

    // Decide: render below or above cursor
    let space_below = screen_rows - cursor_row - 1;
    let render_above = space_below < popup_height + 1;
    let start_row = if render_above {
        cursor_row.saturating_sub(popup_height + 1)
    } else {
        cursor_row + 1
    };

    // Begin synchronized output (prevents flicker)
    write!(stdout, "\x1b[?2026h")?;

    // Save cursor
    write!(stdout, "\x1b7")?;

    // Draw popup border and content
    for (i, suggestion) in suggestions.iter().take(MAX_VISIBLE).enumerate() {
        let row = start_row + i as u16;
        write!(stdout, "\x1b[{};{}H", row + 1, cursor_col + 1)?;

        if i == selected {
            // Highlighted: reverse video + bold
            write!(stdout, "\x1b[1;7m")?;
        } else {
            write!(stdout, "\x1b[0m")?;
        }

        write!(stdout, " {} ", suggestion.display_text)?;
        write!(stdout, "\x1b[0m")?;
    }

    // Restore cursor
    write!(stdout, "\x1b8")?;

    // End synchronized output
    write!(stdout, "\x1b[?2026l")?;

    stdout.flush()?;
    Ok(())
}
```

**Popup cleanup strategy:**
When the popup is dismissed, overwrite its area with spaces (same background as terminal), then restore cursor. This avoids the scrollback corruption that plagues Fig/Amazon Q.

**Handling edge cases:**
- Cursor near bottom of screen: render popup above cursor
- Cursor near right edge: shift popup left
- Terminal resize while popup visible: recalculate and re-render
- Long suggestions: truncate with ellipsis
- Prompt on last line: use alternate positioning

---

### Phase 6: Keybinding and Input Handling (Weeks 10–12, parallel with Phase 5)

**Goal:** Natural keybindings that don't interfere with shell operation.

**Keybinding map:**

| Key | Action (popup visible) | Action (popup hidden) |
|-----|----------------------|---------------------|
| Tab | Accept selected suggestion | Forward to shell (normal tab completion) |
| ↑ / ↓ | Navigate suggestions | Forward to shell |
| Enter | Accept and execute | Forward to shell |
| Escape | Dismiss popup | Forward to shell |
| Ctrl+Space | Force show suggestions | Force show suggestions |
| Any printable | Update filter, re-rank | Forward to shell, check trigger |

**Trigger conditions** (when to show popup):
- After typing a space following a known command
- After typing `/` (path completion)
- After typing `-` or `--` (flag completion)
- After a configurable delay (default: 150ms) with minimum 2 characters
- Manually via Ctrl+Space

---

### Phase 7: Configuration and Polish (Weeks 13–14)

**Goal:** User-configurable behavior, themes, and keybindings.

**Config file:** `~/.config/ghost-complete/config.toml`

```toml
[general]
trigger_delay_ms = 150
max_suggestions = 10
min_chars = 2

[keybindings]
accept = "Tab"
dismiss = "Escape"
force_show = "Ctrl+Space"
navigate_up = "Up"
navigate_down = "Down"

[theme]
border_style = "rounded"     # "rounded", "sharp", "none"
highlight_fg = "#ffffff"
highlight_bg = "#4a9eff"
text_fg = "#cccccc"
text_bg = "#1e1e1e"
border_fg = "#555555"
description_fg = "#888888"

[shell]
default = "zsh"
history_file = "~/.zsh_history"

[completions]
spec_dirs = ["~/.config/ghost-complete/specs", "/usr/share/ghost-complete/specs"]
enable_filesystem = true
enable_git = true
enable_history = true
enable_commands = true
```

---

### Phase 8: Testing and Hardening (Weeks 15–16)

**Testing strategy:**

1. **Unit tests:** Each crate independently tested
   - VT parser: feed known escape sequences, verify state
   - Fuzzy matcher: verify ranking against known inputs
   - Buffer tracker: verify command context detection

2. **Integration tests:** Recorded terminal sessions
   - Capture real PTY sessions as byte streams
   - Replay through proxy, verify state tracking
   - Verify popup rendering at correct positions

3. **Terminal compatibility:**

   | Terminal | Min Version | Status |
   |----------|------------|--------|
   | Ghostty | 1.0+ | Primary (only) target |
   | Others | — | May work, not tested |

4. **Stress tests:**
   - `ls -la /usr/` with 10k+ entries
   - Rapid typing (simulated 120 WPM)
   - Long-running sessions (memory leak detection)
   - Nested shells, tmux/zellij inside proxy

---

## Performance Targets

| Metric | Target | Stretch |
|--------|--------|---------|
| Keystroke → suggestion displayed | < 50ms | < 20ms |
| PTY forwarding latency overhead | < 1ms | < 0.5ms |
| Fuzzy match (10k candidates) | < 5ms | < 1ms |
| Popup render time | < 5ms | < 2ms |
| Memory usage (idle) | < 10 MB | < 5 MB |
| Memory usage (active, 10k candidates) | < 30 MB | < 15 MB |
| Startup time | < 100ms | < 50ms |

---

## Terminal Compatibility: Rendering Features

Ghostty is the only target. It supports everything we need — no fallbacks required.

| Feature | Ghostty |
|---------|---------|
| Cursor save/restore (DECSC/DECRC) | Yes |
| Absolute cursor positioning (CUP) | Yes |
| Synchronized output (DECSET 2026) | Yes |
| OSC 133 (semantic prompts) | Yes |
| OSC 7 (CWD reporting) | Yes |
| Kitty keyboard protocol | Yes |
| 24-bit color (SGR) | Yes |

Other terminals (Kitty, WezTerm) would likely work with minimal effort since they support most of these features, but they are not tested or officially targeted.

---

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| PTY proxy breaks interactive programs (vim, htop) | High | Transparent passthrough by default; only intercept when popup active. Extensive testing with ncurses apps. |
| Scrollback corruption from popup rendering | High | Use DECSET 2026 synchronized output; save/restore cursor with DECSC/DECRC; targeted line clearing instead of full-screen erase. |
| Performance overhead noticeable during fast typing | Medium | Debounce suggestion computation (150ms default); async I/O forwarding on dedicated tokio tasks; zero-copy where possible. |
| Shell integration scripts conflict with user's config | Medium | Minimal hooks; use OSC sequences instead of modifying shell behavior; provide opt-out for each hook. |
| Ghostty-specific assumptions break in future versions | Low | Pin to Ghostty 1.0+ feature set; all features used are standard VT/xterm sequences, not Ghostty-proprietary. |
| Memory leaks in long-running sessions | Medium | Use arena allocation for suggestion candidates; periodic cleanup; integration tests with memory profiling. |

---

## Build Priority

Start with the PTY proxy and filesystem completions. Get `cd <tab>` working smoothly before touching Fig specs, git completions, or any of the fancy stuff. If the core typing experience feels wrong — even slightly — nothing else matters.

Phase 1 (PTY proxy) is the foundation everything else depends on. Get it transparent, get it fast, then layer features on top.

## Open Questions

1. **Should we support Fig/Kiro completion specs directly?** The `@withfig/autocomplete` npm package has 600+ command specs. Supporting their JSON format gives us instant coverage for common tools. Tradeoff: their format is complex and may require a JS runtime for generators.

2. **AI-powered completions?** Integration with local LLMs (via Ollama) or cloud APIs for natural language command suggestions. Interesting differentiator but adds complexity and latency. Punt to v2.

3. **Should the proxy be the default shell wrapper?** For personal use, adding `exec ghost-complete -- zsh` to `.zshrc` or configuring Ghostty's command to launch through the proxy are both viable.

---

## Getting Started

```bash
# Clone and build
git clone https://github.com/user/ghost-complete
cd ghost-complete
cargo build --release

# Run (wraps your default shell)
./target/release/ghost-complete

# Or specify a shell
./target/release/ghost-complete -- /bin/zsh

# Install shell integration (optional, enables richer features)
ghost-complete init zsh >> ~/.zshrc
```

---

## Timeline Summary

| Phase | Duration | Deliverable |
|-------|----------|-------------|
| 1. PTY Proxy Foundation | 2 weeks | Transparent shell wrapper |
| 2. Terminal State Tracking | 2 weeks | Cursor + prompt tracking |
| 3. Command Buffer Tracking | 2 weeks | Command context awareness |
| 4. Suggestion Engine | 3 weeks | Multi-source fuzzy completions |
| 5. Popup Overlay Rendering | 3 weeks | ANSI-based floating popup |
| 6. Keybinding Handling | (parallel with 5) | Input interception |
| 7. Configuration & Polish | 2 weeks | User customization |
| 8. Testing & Hardening | 2 weeks | Cross-terminal validation |
| **Total** | **~14–16 weeks** | **Production-ready v1.0** |
