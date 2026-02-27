# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Ghost Complete** is a personal terminal-native autocomplete engine using PTY proxying, built in Rust. Inspired by Fig (RIP), built for Ghostty. It sits inside the terminal's data stream as a PTY proxy, intercepting I/O between the terminal emulator and the shell. Popups are rendered using native ANSI sequences — no macOS Accessibility API or IME hacks.

The full design lives in `docs/IMPLEMENTATION_PLAN.md`.

## Build & Development Commands

```bash
cargo build                           # Debug build
cargo build --release                 # Release build
cargo test                            # Run all workspace tests
cargo test -p gc-pty                  # Run tests for a single crate
cargo test -p gc-parser -- test_name  # Run a single test
cargo clippy --all-targets            # Lint
cargo fmt --check                     # Check formatting
cargo fmt                             # Auto-format
cargo run -- /bin/zsh                 # Run proxy wrapping zsh
cargo run                             # Run proxy wrapping default shell
```

## Architecture

Rust workspace with 7 crates under `crates/`:

| Crate            | Role                                                                                                                                               |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ghost-complete` | Binary entry point, CLI parsing (clap), daemon launch                                                                                              |
| `gc-pty`         | PTY proxy event loop — spawns shell via `portable-pty`, multiplexes stdin/stdout with `tokio::select!`, handles `SIGWINCH` resize                  |
| `gc-parser`      | VT escape sequence parsing via `vte` crate's `Perform` trait — tracks cursor position, screen dimensions, prompt boundaries (OSC 133), CWD (OSC 7) |
| `gc-buffer`      | Reconstructs current command line buffer, detects command context (which cmd, which arg position), handles pipes/redirects                         |
| `gc-suggest`     | Suggestion engine — dispatches to providers (filesystem, git, history, $PATH commands, Fig-compatible JSON specs), fuzzy-ranks with `nucleo`       |
| `gc-overlay`     | ANSI-based popup rendering — cursor save/restore, synchronized output (DECSET 2026), intelligent above/below positioning, scrollback-safe cleanup  |
| `gc-config`      | TOML config (`~/.config/ghost-complete/config.toml`), keybinding definitions, color themes via `serde`                                             |

### Data Flow

1. User keystroke arrives on stdin
2. `gc-pty` receives it — if popup visible, intercept navigation keys; otherwise forward to shell PTY
3. Shell output flows through `gc-parser` (VT state tracking) then to terminal stdout
4. On trigger conditions (space after command, `/`, `-`, `--`, Ctrl+Space, or delay), `gc-suggest` computes ranked suggestions
5. `gc-overlay` renders popup via ANSI sequences wrapped in synchronized output

### Key Dependency Choices

- **`portable-pty`** (not raw nix/libc): Same PTY abstraction WezTerm uses. Handles PTY pair creation, shell spawning, SIGWINCH in ~5 lines vs ~200 lines of unsafe code.
- **`vte`** (not vt100/alacritty_terminal): Parser-only — fires callbacks per sequence without maintaining a full screen buffer. We only need cursor position + prompt boundaries, not a grid.
- **`nucleo`** (not skim/fuzzy-matcher): ~6x faster than skim. Presegments Unicode once, reuses across queries. <1ms on 10k candidates — critical for the <20ms keystroke-to-suggestion target.
- **`tokio`**: `select!` for PTY I/O multiplexing, `tokio::signal` for SIGWINCH/SIGTERM, `tokio::fs` for async filesystem completions.

## Shell Integration

Scripts in `shell/` provide optional richer features via OSC 133 semantic prompt markers:

- `ghost-complete.zsh` — precmd/preexec hooks
- `ghost-complete.bash` — PROMPT_COMMAND
- `ghost-complete.fish` — event handlers

Without shell integration, features are limited. Primary development targets zsh on Ghostty.

## Target Environment

- **Terminal:** Ghostty only. No fallback heuristics for other terminals.
- **Shell:** zsh (primary). Bash/fish scripts exist but are not actively tested.
- **OS:** macOS
- Ghostty supports all required features: DECSET 2026, OSC 133, OSC 7, Kitty keyboard protocol, 24-bit color.
- Popup rendering must never corrupt scrollback. Use targeted line clearing + cursor save/restore, not full-screen erase.

## Performance Targets

- Keystroke to suggestion: <50ms (stretch: <20ms)
- PTY forwarding overhead: <1ms
- Fuzzy match on 10k candidates: <1ms (nucleo)
- Memory idle: <10MB
- Startup: <100ms

## Completion Specs

Fig-compatible JSON specs live in `specs/` (git, docker, cargo, npm, kubectl). Format documented in `docs/COMPLETION_SPEC.md`.
