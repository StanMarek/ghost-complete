# 0001. PTY proxy over shell plugin

- **Status:** Accepted
- **Date:** 2026-03-01 (retrospective; design predates this ADR)
- **Supersedes:** —
- **Superseded by:** —

## Context

Ghost Complete needs to render a suggestion popup near the shell's cursor on
every keystroke, across multiple terminals (Ghostty, Kitty, WezTerm, Alacritty,
Rio, iTerm2, Terminal.app) and multiple shells (primarily zsh; bash and fish
for manual trigger only). There are three broad ways to get a pop-up near the
cursor:

1. **macOS Accessibility API / IME integration** — what Fig originally did.
   Requires the user to grant accessibility permissions, runs outside the
   terminal process, and is brittle across terminal window-compositing
   differences. `CLAUDE.md:7` explicitly rules this out: "no macOS
   Accessibility API or IME hacks."
2. **Shell plugin** — a zle widget for zsh, a `READLINE_LINE` hook for bash,
   an event handler for fish. Works well for a single shell but couples the
   product to each shell's prompt-rendering and widget machinery (RPROMPT
   alignment, `zle-line-pre-redraw` ordering, `PROMPT_COMMAND` chaining).
3. **PTY proxy** — sit between the terminal emulator and the shell, read every
   byte in both directions, render overlays with raw ANSI escape sequences.

The fundamental constraint is that the rendering layer must know where the
cursor is with byte-level precision and must not corrupt scrollback. It also
has to work the same way in zsh, bash, and fish without rewriting the popup
engine three times.

## Decision

Run as a **PTY proxy**. `gc-pty` spawns the user's shell inside a
`portable-pty` PTY pair, multiplexes stdin/stdout with `tokio::select!`, and
feeds all shell output through a VT parser before forwarding to the terminal
(see `docs/ARCHITECTURE.md:50-58`, "Data Flow"). The proxy itself emits the
popup as ANSI sequences interleaved with the forwarded byte stream.

Shell integration scripts in `shell/` are **optional enhancements**, not
requirements. They emit OSC 133 / OSC 7771 prompt markers so the parser can
detect prompt boundaries precisely. Without them, prompt boundary detection
falls back to heuristics and Ctrl+/ remains the only reliable trigger.

## Consequences

### Positive

- **Shell-agnostic core.** The same proxy runs under zsh, bash, and fish
  (`CLAUDE.md:112-117`). New shells need a small integration script, not a
  rewrite of the rendering engine.
- **No accessibility permissions, no IME hooks.** Install is one binary plus
  one line in `.zshrc`; users never see a macOS TCC prompt for our tool.
- **No zle widget conflicts.** Popup rendering is independent of the shell
  prompt. We do not hook `zle-line-pre-redraw`, touch `RPROMPT`, or chain into
  `PROMPT_COMMAND` in a way that interferes with Powerlevel10k, z4h, or
  Starship (see `docs/ARCHITECTURE.md:97-106`).
- **One binary, no plugin manager.** Users do not need Oh My Zsh / zinit /
  fisher for Ghost Complete to work — `ghost-complete install` is enough.

### Negative

- **We own VT parsing.** The proxy cannot ask the shell where the cursor is;
  it must track cursor state from the byte stream itself. This pushes
  complexity into `gc-parser` (CSI / OSC / DCS handling) and into CPR
  (`CSI 6n`) reconciliation to correct cursor drift. Trade-off accepted in
  ADR-0002.
- **Prompt boundary heuristics without shell integration.** OSC 133 / OSC 7771
  give us exact prompt starts; without them we guess, and guesses are worse on
  multi-line prompts. Our install script wires shell integration automatically
  on zsh, which is the primary target.
- **Process tree has an extra layer.** Tools that walk the process tree or
  rely on `SHELL` being the immediate parent may see `ghost-complete` instead.
  We mitigate this by exec-ing the shell, preserving signals (SIGWINCH,
  SIGTERM), and inheriting CWD/env verbatim.
- **Recursion hazard in tmux / nested sessions.** A naive init block can spawn
  another `ghost-complete` inside an already-proxied shell. We guard with a
  PPID check (v0.1.3, `CHANGELOG.md`) and a per-pane detection in v0.6.1.

## Alternatives considered

- **Accessibility / IME overlay** (Fig, Amazon Q / Kiro style). Rejected for
  reasons in Context above. The README FAQ calls out that popup-alignment
  issues reported against those tools do not apply here, because we render
  inside the terminal grid.
- **Pure zsh plugin.** Rejected because it would (a) leave bash and fish
  unsupported, (b) couple the rendering layer to zle internals that change
  between zsh versions and prompt frameworks, and (c) not generalise to a
  cross-shell suggestion engine.

## References

- `CLAUDE.md:7` — "sits inside the terminal's data stream as a PTY proxy … no
  macOS Accessibility API or IME hacks"
- `docs/ARCHITECTURE.md:3-4` — overview paragraph
- `docs/ARCHITECTURE.md:97-106` — "PTY Proxy over Shell Plugin" design note
- `CHANGELOG.md` entry for v0.1.0 — initial PTY proxy engine shipping
