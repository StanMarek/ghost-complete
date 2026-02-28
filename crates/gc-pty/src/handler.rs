use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    trigger_requested: bool,
}

impl InputHandler {
    pub fn new(spec_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            engine: SuggestionEngine::new(spec_dir)?,
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
            trigger_requested: false,
        })
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn has_pending_trigger(&self) -> bool {
        self.trigger_requested
    }

    pub fn clear_trigger_request(&mut self) {
        self.trigger_requested = false;
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
                Vec::new()
            }
            KeyEvent::ArrowDown => {
                self.overlay.move_down(self.suggestions.len());
                self.render(parser, stdout);
                Vec::new()
            }
            KeyEvent::Tab => {
                let forward = self.accept_suggestion(parser);
                self.dismiss(stdout);
                forward
            }
            KeyEvent::Enter => {
                let mut forward = self.accept_suggestion(parser);
                self.dismiss(stdout);
                forward.push(0x0D);
                forward
            }
            KeyEvent::Escape => {
                self.dismiss(stdout);
                Vec::new()
            }
            KeyEvent::ArrowLeft | KeyEvent::ArrowRight => {
                self.dismiss(stdout);
                key_to_bytes(key)
            }
            KeyEvent::Printable(_) | KeyEvent::Backspace => {
                let forward = key_to_bytes(key);
                // Defer re-trigger to Task B after shell updates buffer
                self.trigger_requested = true;
                forward
            }
            _ => {
                self.dismiss(stdout);
                key_to_bytes(key)
            }
        }
    }

    fn process_key_hidden(
        &mut self,
        key: &KeyEvent,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) -> Vec<u8> {
        match key {
            KeyEvent::CtrlSpace => {
                // Manual trigger — fire immediately (no PTY roundtrip needed)
                self.trigger(parser, stdout);
                Vec::new()
            }
            KeyEvent::Printable(c) => {
                let forward = vec![*c as u8];
                if should_trigger_on_char(*c) {
                    // Defer trigger to Task B after shell processes the keystroke
                    self.trigger_requested = true;
                }
                forward
            }
            _ => key_to_bytes(key),
        }
    }

    pub fn trigger(&mut self, parser: &Arc<Mutex<TerminalParser>>, stdout: &mut dyn Write) {
        let (buffer, cursor, cwd, cursor_row, cursor_col, screen_rows, screen_cols) = {
            let p = parser.lock().unwrap();
            let state = p.state();
            let buffer = state.command_buffer().unwrap_or("").to_string();
            let cursor = state.buffer_cursor();
            let cwd = state.cwd().cloned().unwrap_or_else(|| PathBuf::from("."));
            let (cursor_row, cursor_col) = state.cursor_position();
            let (screen_rows, screen_cols) = state.screen_dimensions();
            (
                buffer,
                cursor,
                cwd,
                cursor_row,
                cursor_col,
                screen_rows,
                screen_cols,
            )
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

    fn render(&mut self, parser: &Arc<Mutex<TerminalParser>>, stdout: &mut dyn Write) {
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

    fn accept_suggestion(&self, parser: &Arc<Mutex<TerminalParser>>) -> Vec<u8> {
        if self.suggestions.is_empty() {
            return Vec::new();
        }

        let selected = &self.suggestions[self.overlay.selected];

        let current_word_len = {
            let p = parser.lock().unwrap();
            let state = p.state();
            let buffer = state.command_buffer().unwrap_or("");
            let cursor = state.buffer_cursor();
            let ctx = parse_command_context(buffer, cursor);
            ctx.current_word.len()
        };

        let mut bytes = vec![0x7F; current_word_len];

        // Type the suggestion text
        bytes.extend_from_slice(selected.text.as_bytes());

        bytes
    }

    /// Handle terminal resize while popup is visible.
    pub fn handle_resize(&mut self, parser: &Arc<Mutex<TerminalParser>>, stdout: &mut dyn Write) {
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
pub fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
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
    use gc_suggest::{SuggestionKind, SuggestionSource};

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
                kind: SuggestionKind::Command,
                source: SuggestionSource::Commands,
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
            trigger_requested: false,
        };

        let mut stdout_buf = Vec::new();
        handler.dismiss(&mut stdout_buf);

        assert!(!handler.visible);
        assert!(handler.suggestions.is_empty());
        assert!(handler.last_layout.is_none());
        assert!(!stdout_buf.is_empty());
    }

    fn make_handler() -> InputHandler {
        InputHandler {
            engine: SuggestionEngine::new(Path::new(".")).unwrap(),
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
            trigger_requested: false,
        }
    }

    #[test]
    fn test_trigger_requested_on_space() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::Printable(' '), &parser, &mut buf);
        assert!(handler.has_pending_trigger());
    }

    #[test]
    fn test_trigger_not_requested_on_alpha() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::Printable('a'), &parser, &mut buf);
        assert!(!handler.has_pending_trigger());
    }

    #[test]
    fn test_ctrl_space_triggers_immediately() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::CtrlSpace, &parser, &mut buf);
        // CtrlSpace triggers immediately — does NOT set trigger_requested
        assert!(!handler.has_pending_trigger());
    }

    #[test]
    fn test_handler_starts_not_visible() {
        let handler = InputHandler {
            engine: SuggestionEngine::new(Path::new(".")).unwrap(),
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
            trigger_requested: false,
        };
        assert!(!handler.is_visible());
        assert!(!handler.has_pending_trigger());
    }
}
