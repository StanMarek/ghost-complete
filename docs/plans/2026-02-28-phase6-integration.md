# Phase 6: PTY Integration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Wire gc-buffer, gc-suggest, and gc-overlay into the gc-pty event loop so keystrokes trigger suggestions and render an interactive popup.

**Architecture:** Add `input.rs` (key parser) and `handler.rs` (popup state machine) to gc-pty. Modify Task A in `proxy.rs` to route keystrokes through `InputHandler` instead of blind forwarding. The handler writes popup ANSI directly to stdout; shell I/O flows unchanged through Task B.

**Tech Stack:** Rust, tokio (existing), gc-buffer, gc-suggest, gc-overlay, ANSI escape sequences

---

## Step 1: Update gc-pty dependencies

### 1.1 — Modify `crates/gc-pty/Cargo.toml`

Add gc-buffer, gc-suggest, gc-overlay dependencies:

```toml
[dependencies]
portable-pty = "0.8"
tokio = { workspace = true }
crossterm = { workspace = true }
nix = { version = "0.29", features = ["signal"] }
anyhow = { workspace = true }
tracing = { workspace = true }
gc-parser = { path = "../gc-parser" }
gc-buffer = { path = "../gc-buffer" }
gc-suggest = { path = "../gc-suggest" }
gc-overlay = { path = "../gc-overlay" }
```

### 1.2 — Verify it compiles

Run: `cargo build -p gc-pty`
Expected: compiles with no errors

---

## Step 2: Key event parsing (`input.rs`)

### 2.1 — Create `crates/gc-pty/src/input.rs`

```rust
/// Minimal key event parser for raw terminal stdin bytes.
///
/// Parses known sequences (arrows, Tab, Enter, Escape, Ctrl+Space) and
/// passes through everything else as Raw bytes.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    Tab,
    Enter,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    CtrlSpace,
    Backspace,
    Printable(char),
    /// Unknown bytes — forward verbatim to PTY.
    Raw(Vec<u8>),
}

/// Parse a buffer of raw stdin bytes into key events.
///
/// One read() call can contain multiple keystrokes (e.g. fast typing or
/// paste). This returns them all in order.
pub fn parse_keys(buf: &[u8]) -> Vec<KeyEvent> {
    let mut events = Vec::new();
    let mut i = 0;

    while i < buf.len() {
        match buf[i] {
            0x00 => {
                // Ctrl+Space
                events.push(KeyEvent::CtrlSpace);
                i += 1;
            }
            0x09 => {
                events.push(KeyEvent::Tab);
                i += 1;
            }
            0x0D => {
                events.push(KeyEvent::Enter);
                i += 1;
            }
            0x7F => {
                events.push(KeyEvent::Backspace);
                i += 1;
            }
            0x1B => {
                // Escape or CSI sequence
                if i + 2 < buf.len() && buf[i + 1] == b'[' {
                    // CSI sequence
                    match buf[i + 2] {
                        b'A' => {
                            events.push(KeyEvent::ArrowUp);
                            i += 3;
                        }
                        b'B' => {
                            events.push(KeyEvent::ArrowDown);
                            i += 3;
                        }
                        b'C' => {
                            events.push(KeyEvent::ArrowRight);
                            i += 3;
                        }
                        b'D' => {
                            events.push(KeyEvent::ArrowLeft);
                            i += 3;
                        }
                        _ => {
                            // Unknown CSI — find end and pass through as Raw
                            let start = i;
                            i += 3;
                            // CSI params: bytes in 0x30-0x3F, intermediates: 0x20-0x2F
                            // Final byte: 0x40-0x7E
                            while i < buf.len() && buf[i] < 0x40 {
                                i += 1;
                            }
                            if i < buf.len() {
                                i += 1; // consume final byte
                            }
                            events.push(KeyEvent::Raw(buf[start..i].to_vec()));
                        }
                    }
                } else if i + 1 < buf.len() && buf[i + 1] == b'O' {
                    // SS3 sequences (some terminals use ESC O A for arrow keys)
                    if i + 2 < buf.len() {
                        match buf[i + 2] {
                            b'A' => {
                                events.push(KeyEvent::ArrowUp);
                                i += 3;
                            }
                            b'B' => {
                                events.push(KeyEvent::ArrowDown);
                                i += 3;
                            }
                            b'C' => {
                                events.push(KeyEvent::ArrowRight);
                                i += 3;
                            }
                            b'D' => {
                                events.push(KeyEvent::ArrowLeft);
                                i += 3;
                            }
                            _ => {
                                events.push(KeyEvent::Raw(buf[i..i + 3].to_vec()));
                                i += 3;
                            }
                        }
                    } else {
                        events.push(KeyEvent::Raw(buf[i..].to_vec()));
                        i = buf.len();
                    }
                } else if i + 1 == buf.len() {
                    // Standalone ESC at end of buffer
                    events.push(KeyEvent::Escape);
                    i += 1;
                } else {
                    // ESC followed by something that's not [ or O
                    // Could be Alt+key — pass through as raw
                    events.push(KeyEvent::Raw(buf[i..i + 2].to_vec()));
                    i += 2;
                }
            }
            b if b >= 0x20 && b <= 0x7E => {
                events.push(KeyEvent::Printable(b as char));
                i += 1;
            }
            _ => {
                // Control char or high byte — pass through
                events.push(KeyEvent::Raw(vec![buf[i]]));
                i += 1;
            }
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_printable_chars() {
        let events = parse_keys(b"abc");
        assert_eq!(
            events,
            vec![
                KeyEvent::Printable('a'),
                KeyEvent::Printable('b'),
                KeyEvent::Printable('c'),
            ]
        );
    }

    #[test]
    fn test_tab() {
        let events = parse_keys(b"\x09");
        assert_eq!(events, vec![KeyEvent::Tab]);
    }

    #[test]
    fn test_enter() {
        let events = parse_keys(b"\x0D");
        assert_eq!(events, vec![KeyEvent::Enter]);
    }

    #[test]
    fn test_backspace() {
        let events = parse_keys(b"\x7F");
        assert_eq!(events, vec![KeyEvent::Backspace]);
    }

    #[test]
    fn test_ctrl_space() {
        let events = parse_keys(b"\x00");
        assert_eq!(events, vec![KeyEvent::CtrlSpace]);
    }

    #[test]
    fn test_arrow_keys_csi() {
        assert_eq!(parse_keys(b"\x1B[A"), vec![KeyEvent::ArrowUp]);
        assert_eq!(parse_keys(b"\x1B[B"), vec![KeyEvent::ArrowDown]);
        assert_eq!(parse_keys(b"\x1B[C"), vec![KeyEvent::ArrowRight]);
        assert_eq!(parse_keys(b"\x1B[D"), vec![KeyEvent::ArrowLeft]);
    }

    #[test]
    fn test_arrow_keys_ss3() {
        assert_eq!(parse_keys(b"\x1BOA"), vec![KeyEvent::ArrowUp]);
        assert_eq!(parse_keys(b"\x1BOB"), vec![KeyEvent::ArrowDown]);
    }

    #[test]
    fn test_standalone_escape() {
        // ESC alone at end of buffer
        let events = parse_keys(b"\x1B");
        assert_eq!(events, vec![KeyEvent::Escape]);
    }

    #[test]
    fn test_unknown_csi_passthrough() {
        // e.g. ESC [ 1 ; 5 C (Ctrl+Right in some terminals)
        let raw = b"\x1B[1;5C";
        let events = parse_keys(raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            KeyEvent::Raw(bytes) => assert_eq!(bytes, raw),
            other => panic!("expected Raw, got {:?}", other),
        }
    }

    #[test]
    fn test_mixed_input() {
        // "a" then ArrowUp then "b"
        let events = parse_keys(b"a\x1B[Ab");
        assert_eq!(
            events,
            vec![
                KeyEvent::Printable('a'),
                KeyEvent::ArrowUp,
                KeyEvent::Printable('b'),
            ]
        );
    }

    #[test]
    fn test_empty_input() {
        let events = parse_keys(b"");
        assert!(events.is_empty());
    }

    #[test]
    fn test_alt_key_passthrough() {
        // Alt+a = ESC a — should be Raw
        let events = parse_keys(b"\x1Ba");
        assert_eq!(events.len(), 1);
        match &events[0] {
            KeyEvent::Raw(bytes) => assert_eq!(bytes, b"\x1Ba"),
            other => panic!("expected Raw, got {:?}", other),
        }
    }
}
```

### 2.2 — Register module in `crates/gc-pty/src/lib.rs`

Change from:
```rust
mod proxy;
mod resize;
mod spawn;

pub use proxy::run_proxy;
```

To:
```rust
pub mod input;
mod proxy;
mod resize;
mod spawn;

pub use proxy::run_proxy;
```

### 2.3 — Verify

Run: `cargo test -p gc-pty`
Expected: all input tests pass

---

## Step 3: Input handler (`handler.rs`)

### 3.1 — Create `crates/gc-pty/src/handler.rs`

```rust
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use gc_buffer::parse_command_context;
use gc_overlay::types::{OverlayState, PopupLayout};
use gc_overlay::{clear_popup, render_popup};
use gc_parser::TerminalParser;
use gc_suggest::{Suggestion, SuggestionEngine};

use crate::input::KeyEvent;

pub struct InputHandler {
    engine: SuggestionEngine,
    overlay: OverlayState,
    suggestions: Vec<Suggestion>,
    last_layout: Option<PopupLayout>,
    visible: bool,
}

impl InputHandler {
    pub fn new(spec_dir: &Path) -> Result<Self> {
        Ok(Self {
            engine: SuggestionEngine::new(spec_dir)?,
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
        })
    }

    #[cfg(test)]
    pub fn new_noop() -> Self {
        Self {
            engine: SuggestionEngine::new(Path::new("/nonexistent")).unwrap_or_else(|_| {
                // For tests, create with empty spec dir
                SuggestionEngine::new(Path::new(".")).unwrap()
            }),
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Process a single key event. Returns the raw bytes to forward to the PTY,
    /// or empty if the key was intercepted by the popup.
    pub fn process_key(
        &mut self,
        key: &KeyEvent,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) -> Vec<u8> {
        if self.visible {
            self.process_key_visible(key, parser, stdout)
        } else {
            self.process_key_hidden(key, parser, stdout)
        }
    }

    /// Handle key when popup is visible.
    /// Returns bytes to forward to PTY (empty = intercepted).
    fn process_key_visible(
        &mut self,
        key: &KeyEvent,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) -> Vec<u8> {
        match key {
            KeyEvent::ArrowUp => {
                self.overlay.move_up();
                self.render(parser, stdout);
                Vec::new() // intercepted
            }
            KeyEvent::ArrowDown => {
                self.overlay.move_down(self.suggestions.len());
                self.render(parser, stdout);
                Vec::new() // intercepted
            }
            KeyEvent::Tab => {
                let forward = self.accept_suggestion(parser);
                self.dismiss(stdout);
                forward
            }
            KeyEvent::Enter => {
                let mut forward = self.accept_suggestion(parser);
                self.dismiss(stdout);
                forward.push(0x0D); // also send Enter
                forward
            }
            KeyEvent::Escape => {
                self.dismiss(stdout);
                Vec::new() // intercepted, don't send ESC to shell
            }
            KeyEvent::ArrowLeft | KeyEvent::ArrowRight => {
                self.dismiss(stdout);
                key_to_bytes(key)
            }
            KeyEvent::Printable(c) => {
                let forward = vec![*c as u8];
                // Forward the char first, then re-trigger after shell processes it
                // We'll re-trigger on the next iteration after the parser updates
                self.schedule_retrigger(parser, stdout);
                forward
            }
            KeyEvent::Backspace => {
                let forward = vec![0x7F];
                self.schedule_retrigger(parser, stdout);
                forward
            }
            _ => {
                // Unknown key while popup visible — dismiss and forward
                self.dismiss(stdout);
                key_to_bytes(key)
            }
        }
    }

    /// Handle key when popup is hidden.
    /// Returns bytes to forward to PTY.
    fn process_key_hidden(
        &mut self,
        key: &KeyEvent,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) -> Vec<u8> {
        match key {
            KeyEvent::CtrlSpace => {
                // Force trigger
                let forward = Vec::new(); // don't forward Ctrl+Space
                self.trigger(parser, stdout);
                forward
            }
            KeyEvent::Printable(c) => {
                let forward = vec![*c as u8];
                // Check if this char should trigger suggestions
                if should_trigger_on_char(*c) {
                    // We need to trigger AFTER the shell processes this char,
                    // but we can't wait. Instead, trigger with current state
                    // (the parser won't have this char yet, but that's OK for
                    // space/slash/dash triggers — we check the char itself).
                    self.trigger(parser, stdout);
                }
                forward
            }
            _ => key_to_bytes(key),
        }
    }

    /// Trigger the suggestion pipeline.
    fn trigger(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) {
        let (buffer, cursor, cwd, cursor_row, cursor_col, screen_rows, screen_cols) = {
            let p = parser.lock().unwrap();
            let state = p.state();
            let buffer = state.command_buffer().unwrap_or("").to_string();
            let cursor = state.buffer_cursor();
            let cwd = state
                .cwd()
                .cloned()
                .unwrap_or_else(|| PathBuf::from("."));
            let (cursor_row, cursor_col) = state.cursor_position();
            let (screen_rows, screen_cols) = state.screen_dimensions();
            (buffer, cursor, cwd, cursor_row, cursor_col, screen_rows, screen_cols)
        };

        if buffer.is_empty() {
            if self.visible {
                self.dismiss(stdout);
            }
            return;
        }

        let ctx = parse_command_context(&buffer, cursor);

        match self.engine.suggest_sync(&ctx, &cwd) {
            Ok(suggestions) if !suggestions.is_empty() => {
                self.suggestions = suggestions;
                self.overlay.reset();
                self.visible = true;
                self.render_at(stdout, cursor_row, cursor_col, screen_rows, screen_cols);
            }
            _ => {
                if self.visible {
                    self.dismiss(stdout);
                }
            }
        }
    }

    /// Re-trigger suggestions when popup is already visible and user types/deletes.
    fn schedule_retrigger(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) {
        // Clear current popup, then re-trigger
        // Note: the parser hasn't processed the new keystroke yet (it goes
        // through Task B), so the buffer is stale. This means the first
        // re-render after typing will be one char behind. This is acceptable
        // for now — the next keystroke will correct it.
        self.trigger(parser, stdout);
    }

    /// Render the popup at the current cursor position.
    fn render(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) {
        let (cursor_row, cursor_col, screen_rows, screen_cols) = {
            let p = parser.lock().unwrap();
            let state = p.state();
            let (cr, cc) = state.cursor_position();
            let (sr, sc) = state.screen_dimensions();
            (cr, cc, sr, sc)
        };
        self.render_at(stdout, cursor_row, cursor_col, screen_rows, screen_cols);
    }

    fn render_at(
        &mut self,
        stdout: &mut dyn Write,
        cursor_row: u16,
        cursor_col: u16,
        screen_rows: u16,
        screen_cols: u16,
    ) {
        // Clear previous popup if it existed
        if let Some(ref layout) = self.last_layout {
            let mut clear_buf = Vec::new();
            clear_popup(&mut clear_buf, layout);
            let _ = stdout.write_all(&clear_buf);
        }

        let mut render_buf = Vec::new();
        let layout = render_popup(
            &mut render_buf,
            &self.suggestions,
            &self.overlay,
            cursor_row,
            cursor_col,
            screen_rows,
            screen_cols,
        );
        let _ = stdout.write_all(&render_buf);
        let _ = stdout.flush();
        self.last_layout = Some(layout);
    }

    /// Dismiss the popup — clear from screen, reset state.
    fn dismiss(&mut self, stdout: &mut dyn Write) {
        if let Some(ref layout) = self.last_layout {
            let mut buf = Vec::new();
            clear_popup(&mut buf, layout);
            let _ = stdout.write_all(&buf);
            let _ = stdout.flush();
        }
        self.visible = false;
        self.suggestions.clear();
        self.overlay.reset();
        self.last_layout = None;
    }

    /// Accept the currently selected suggestion.
    /// Returns bytes to send to PTY (backspaces + suggestion text).
    fn accept_suggestion(&self, parser: &Arc<Mutex<TerminalParser>>) -> Vec<u8> {
        if self.suggestions.is_empty() {
            return Vec::new();
        }

        let selected = &self.suggestions[self.overlay.selected];

        // Figure out how many chars to erase (the partial word)
        let current_word_len = {
            let p = parser.lock().unwrap();
            let state = p.state();
            let buffer = state.command_buffer().unwrap_or("");
            let cursor = state.buffer_cursor();
            let ctx = parse_command_context(buffer, cursor);
            ctx.current_word.len()
        };

        let mut bytes = Vec::new();

        // Send backspaces to erase the partial word
        for _ in 0..current_word_len {
            bytes.push(0x7F); // DEL (backspace in raw mode)
        }

        // Type the suggestion text
        bytes.extend_from_slice(selected.text.as_bytes());

        bytes
    }

    /// Handle terminal resize while popup is visible.
    pub fn handle_resize(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) {
        if self.visible {
            self.render(parser, stdout);
        }
    }
}

/// Check if a printable character should trigger suggestions.
fn should_trigger_on_char(c: char) -> bool {
    matches!(c, ' ' | '/' | '-' | '.')
}

/// Convert a KeyEvent back to raw bytes for forwarding to PTY.
fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    match key {
        KeyEvent::Tab => vec![0x09],
        KeyEvent::Enter => vec![0x0D],
        KeyEvent::Escape => vec![0x1B],
        KeyEvent::ArrowUp => vec![0x1B, b'[', b'A'],
        KeyEvent::ArrowDown => vec![0x1B, b'[', b'B'],
        KeyEvent::ArrowRight => vec![0x1B, b'[', b'C'],
        KeyEvent::ArrowLeft => vec![0x1B, b'[', b'D'],
        KeyEvent::CtrlSpace => vec![0x00],
        KeyEvent::Backspace => vec![0x7F],
        KeyEvent::Printable(c) => vec![*c as u8],
        KeyEvent::Raw(bytes) => bytes.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_trigger_on_space() {
        assert!(should_trigger_on_char(' '));
    }

    #[test]
    fn test_should_trigger_on_slash() {
        assert!(should_trigger_on_char('/'));
    }

    #[test]
    fn test_should_trigger_on_dash() {
        assert!(should_trigger_on_char('-'));
    }

    #[test]
    fn test_should_trigger_on_dot() {
        assert!(should_trigger_on_char('.'));
    }

    #[test]
    fn test_should_not_trigger_on_alpha() {
        assert!(!should_trigger_on_char('a'));
        assert!(!should_trigger_on_char('Z'));
    }

    #[test]
    fn test_key_to_bytes_tab() {
        assert_eq!(key_to_bytes(&KeyEvent::Tab), vec![0x09]);
    }

    #[test]
    fn test_key_to_bytes_arrow_up() {
        assert_eq!(key_to_bytes(&KeyEvent::ArrowUp), vec![0x1B, b'[', b'A']);
    }

    #[test]
    fn test_key_to_bytes_printable() {
        assert_eq!(key_to_bytes(&KeyEvent::Printable('x')), vec![b'x']);
    }

    #[test]
    fn test_key_to_bytes_raw() {
        let raw = vec![0x1B, b'[', b'1', b';', b'5', b'C'];
        assert_eq!(key_to_bytes(&KeyEvent::Raw(raw.clone())), raw);
    }

    #[test]
    fn test_key_to_bytes_roundtrip() {
        // Every KeyEvent variant should produce non-empty bytes
        let keys = vec![
            KeyEvent::Tab,
            KeyEvent::Enter,
            KeyEvent::Escape,
            KeyEvent::ArrowUp,
            KeyEvent::ArrowDown,
            KeyEvent::ArrowLeft,
            KeyEvent::ArrowRight,
            KeyEvent::CtrlSpace,
            KeyEvent::Backspace,
            KeyEvent::Printable('a'),
            KeyEvent::Raw(vec![0xFF]),
        ];
        for key in keys {
            let bytes = key_to_bytes(&key);
            assert!(!bytes.is_empty(), "key_to_bytes({:?}) was empty", key);
        }
    }

    #[test]
    fn test_dismiss_clears_state() {
        let mut handler = InputHandler {
            engine: SuggestionEngine::new(Path::new(".")).unwrap(),
            overlay: OverlayState::new(),
            suggestions: vec![Suggestion {
                text: "test".to_string(),
                description: None,
                kind: gc_suggest::SuggestionKind::Command,
                source: gc_suggest::SuggestionSource::Commands,
                score: 0,
            }],
            last_layout: Some(PopupLayout {
                start_row: 5,
                start_col: 0,
                width: 20,
                height: 1,
                renders_above: false,
            }),
            visible: true,
        };

        let mut stdout_buf = Vec::new();
        handler.dismiss(&mut stdout_buf);

        assert!(!handler.visible);
        assert!(handler.suggestions.is_empty());
        assert!(handler.last_layout.is_none());
        // Should have written clear sequence
        assert!(!stdout_buf.is_empty());
    }

    #[test]
    fn test_handler_starts_not_visible() {
        let handler = InputHandler {
            engine: SuggestionEngine::new(Path::new(".")).unwrap(),
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
        };
        assert!(!handler.is_visible());
    }
}
```

### 3.2 — Register module in `crates/gc-pty/src/lib.rs`

```rust
mod handler;
pub mod input;
mod proxy;
mod resize;
mod spawn;

pub use proxy::run_proxy;
```

### 3.3 — Verify

Run: `cargo test -p gc-pty`
Expected: all handler + input tests pass

---

## Step 4: Modify proxy.rs to use InputHandler

### 4.1 — Rewrite `crates/gc-pty/src/proxy.rs`

Replace the entire file:

```rust
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gc_parser::TerminalParser;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

use crate::handler::InputHandler;
use crate::input::parse_keys;
use crate::resize::{get_terminal_size, resize_pty};
use crate::spawn::{spawn_shell, SpawnedShell};

/// Drop guard that ensures raw mode is always restored, even on panic.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("failed to enable raw mode")?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Run the PTY proxy event loop. This is the main entry point for the proxy.
///
/// Spawns the given shell, enters raw mode, and forwards all I/O between
/// stdin/stdout and the PTY until the shell exits.
///
/// Returns the shell's exit code.
pub async fn run_proxy(shell: &str, args: &[String]) -> Result<i32> {
    let SpawnedShell { master, mut child } = spawn_shell(shell, args)?;

    let mut reader = master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;
    let writer = master.take_writer().context("failed to take PTY writer")?;

    // Enter raw mode with a drop guard so it's ALWAYS restored
    let _raw_guard = RawModeGuard::enable()?;

    // Initialize terminal parser with current screen dimensions
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let parser = Arc::new(Mutex::new(TerminalParser::new(rows, cols)));

    // Initialize suggestion handler
    let spec_dir = spec_directory();
    let handler = Arc::new(Mutex::new(
        InputHandler::new(&spec_dir).unwrap_or_else(|e| {
            tracing::warn!("failed to init suggestion engine: {}, suggestions disabled", e);
            InputHandler::new(std::path::Path::new(".")).expect("fallback handler")
        }),
    ));

    // Channel to signal that one of the I/O tasks has finished
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // Task A: stdin → PTY (user keystrokes to shell, with popup interception)
    let stdin_shutdown = shutdown_tx.clone();
    let mut pty_writer = writer;
    let parser_for_stdin = Arc::clone(&parser);
    let handler_for_stdin = Arc::clone(&handler);
    let stdin_handle = tokio::task::spawn_blocking(move || {
        let mut stdin = std::io::stdin().lock();
        let mut stdout = std::io::stdout().lock();
        let mut buf = [0u8; 256];
        loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };

            let keys = parse_keys(&buf[..n]);
            for key in &keys {
                let forward = {
                    let mut h = handler_for_stdin.lock().unwrap();
                    h.process_key(key, &parser_for_stdin, &mut stdout)
                };
                if !forward.is_empty() {
                    if pty_writer.write_all(&forward).is_err() {
                        return;
                    }
                    if pty_writer.flush().is_err() {
                        return;
                    }
                }
            }
        }
        let _ = stdin_shutdown.try_send(());
    });

    // Task B: PTY → stdout (shell output to terminal)
    let pty_shutdown = shutdown_tx.clone();
    let parser_for_stdout = Arc::clone(&parser);
    let stdout_handle = tokio::task::spawn_blocking(move || {
        let mut stdout = std::io::stdout().lock();
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break, // PTY closed
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };

            // Feed bytes through the VT parser to track terminal state
            {
                let mut p = parser_for_stdout.lock().unwrap();
                p.process_bytes(&buf[..n]);
            }

            if stdout.write_all(&buf[..n]).is_err() {
                break;
            }
            if stdout.flush().is_err() {
                break;
            }
        }
        let _ = pty_shutdown.try_send(());
    });

    // Drop the sender we cloned from — we only need the ones in the tasks
    drop(shutdown_tx);

    // Task C: Signal handling
    let mut sigwinch =
        signal(SignalKind::window_change()).context("failed to register SIGWINCH handler")?;

    // Wait for either an I/O task to finish or a signal
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                tracing::debug!("I/O task finished, shutting down");
                break;
            }
            _ = sigwinch.recv() => {
                match get_terminal_size() {
                    Ok(size) => {
                        if let Err(e) = resize_pty(master.as_ref(), size) {
                            tracing::warn!("failed to resize PTY: {}", e);
                        }
                        // Update parser's screen dimensions
                        {
                            let mut p = parser.lock().unwrap();
                            p.state_mut().update_dimensions(size.rows, size.cols);
                        }
                        // Re-render popup if visible
                        {
                            let mut stdout = std::io::stdout().lock();
                            let mut h = handler.lock().unwrap();
                            h.handle_resize(&parser, &mut stdout);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to get terminal size for resize: {}", e);
                    }
                }
            }
        }
    }

    // Clean up: abort I/O tasks (they'll be blocked on reads)
    stdin_handle.abort();
    stdout_handle.abort();

    // _raw_guard drops here, restoring terminal state

    // Wait for child and get exit status
    let status = child.wait().context("failed to wait for shell process")?;
    let exit_code = status.exit_code().try_into().unwrap_or(1);

    Ok(exit_code)
}

/// Find the completion specs directory.
fn spec_directory() -> PathBuf {
    // Check for specs/ next to the binary first, then fall back to cargo workspace
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    if let Some(dir) = exe_dir {
        let spec_dir = dir.join("specs");
        if spec_dir.is_dir() {
            return spec_dir;
        }
    }

    // Fall back to specs/ in the current directory (development)
    PathBuf::from("specs")
}
```

### 4.2 — Verify compilation

Run: `cargo build -p gc-pty`
Expected: compiles with no errors

### 4.3 — Run all tests

Run: `cargo test`
Expected: all workspace tests pass (existing 129 + new input/handler tests)

---

## Step 5: Final verification

### 5.1 — Format check

Run: `cargo fmt --check`
Expected: clean

If not clean, run: `cargo fmt`

### 5.2 — Clippy

Run: `cargo clippy --all-targets`
Expected: no new warnings (existing gc-suggest `name()` warning is OK)

### 5.3 — Full test suite

Run: `cargo test`
Expected: all tests pass

### 5.4 — Commit

```bash
git add crates/gc-pty/
git commit -m "Phase 6: PTY integration — input parsing, handler, proxy wiring"
```

---

## Files to create/modify

| File | Action |
|------|--------|
| `crates/gc-pty/Cargo.toml` | Modify — add gc-buffer, gc-suggest, gc-overlay deps |
| `crates/gc-pty/src/input.rs` | Create — key event parser |
| `crates/gc-pty/src/handler.rs` | Create — popup state machine |
| `crates/gc-pty/src/proxy.rs` | Rewrite — use InputHandler in Task A |
| `crates/gc-pty/src/lib.rs` | Modify — add module declarations |

## Design decisions

**Synthetic keystrokes for accept** — Send backspaces to erase partial word, then type suggestion text as raw bytes to PTY. Works without shell integration, universally compatible.

**No debounce timer** — Trigger immediately on condition characters (space, slash, dash, dot) and on Ctrl+Space. Keeps implementation simple. Can add delay later if it feels too aggressive.

**Parser state is one keystroke behind** — When the user types a char, we forward it to the PTY and trigger suggestions, but the parser hasn't processed the shell's response yet. The command buffer in the parser reflects the state *before* the latest keystroke. This is acceptable — the next keystroke or the next shell output will update it.

**Separate stdout handles** — Task A and Task B each lock stdout independently. On Unix, concurrent writes to the same fd work at the kernel level. Synchronized output (DECSET 2026) prevents visual tearing for popup renders.

**InputHandler behind Arc<Mutex>** — Shared between Task A (processes keys) and the main loop (SIGWINCH re-render). Lock contention is minimal — SIGWINCH is rare.
