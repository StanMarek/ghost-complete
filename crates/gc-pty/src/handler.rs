use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gc_buffer::{byte_to_char_offset, char_to_byte_offset, parse_command_context};
use gc_overlay::types::{
    OverlayState, PopupLayout, DEFAULT_MAX_POPUP_WIDTH, DEFAULT_MAX_VISIBLE,
    DEFAULT_MIN_POPUP_WIDTH,
};
use gc_overlay::{clear_popup, render_popup, PopupTheme};
use gc_parser::TerminalParser;
use gc_suggest::{Suggestion, SuggestionEngine};

use crate::input::KeyEvent;

/// Resolved keybindings — each action maps to a concrete `KeyEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keybindings {
    pub accept: KeyEvent,
    pub accept_and_enter: KeyEvent,
    pub dismiss: KeyEvent,
    pub navigate_up: KeyEvent,
    pub navigate_down: KeyEvent,
    pub trigger: KeyEvent,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            accept: KeyEvent::Tab,
            accept_and_enter: KeyEvent::Enter,
            dismiss: KeyEvent::Escape,
            navigate_up: KeyEvent::ArrowUp,
            navigate_down: KeyEvent::ArrowDown,
            trigger: KeyEvent::CtrlSlash,
        }
    }
}

impl Keybindings {
    pub fn from_config(config: &gc_config::KeybindingsConfig) -> anyhow::Result<Self> {
        Ok(Self {
            accept: parse_key_name(&config.accept)?,
            accept_and_enter: parse_key_name(&config.accept_and_enter)?,
            dismiss: parse_key_name(&config.dismiss)?,
            navigate_up: parse_key_name(&config.navigate_up)?,
            navigate_down: parse_key_name(&config.navigate_down)?,
            trigger: parse_key_name(&config.trigger)?,
        })
    }
}

/// Parse a human-readable key name into a `KeyEvent`.
///
/// Supported names (case-insensitive):
/// `tab`, `enter`, `escape`, `backspace`, `ctrl+space`, `ctrl+/`,
/// `arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`
pub fn parse_key_name(name: &str) -> anyhow::Result<KeyEvent> {
    match name.trim().to_lowercase().as_str() {
        "tab" => Ok(KeyEvent::Tab),
        "enter" => Ok(KeyEvent::Enter),
        "escape" => Ok(KeyEvent::Escape),
        "backspace" => Ok(KeyEvent::Backspace),
        "ctrl+space" => Ok(KeyEvent::CtrlSpace),
        "ctrl+/" => Ok(KeyEvent::CtrlSlash),
        "arrow_up" => Ok(KeyEvent::ArrowUp),
        "arrow_down" => Ok(KeyEvent::ArrowDown),
        "arrow_left" => Ok(KeyEvent::ArrowLeft),
        "arrow_right" => Ok(KeyEvent::ArrowRight),
        other => anyhow::bail!("unknown key name: {:?}", other),
    }
}

pub struct InputHandler {
    engine: SuggestionEngine,
    overlay: OverlayState,
    suggestions: Vec<Suggestion>,
    last_layout: Option<PopupLayout>,
    visible: bool,
    trigger_requested: bool,
    max_visible: usize,
    min_width: u16,
    max_width: u16,
    trigger_chars: HashSet<char>,
    keybindings: Keybindings,
    theme: PopupTheme,
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
            max_visible: DEFAULT_MAX_VISIBLE,
            min_width: DEFAULT_MIN_POPUP_WIDTH,
            max_width: DEFAULT_MAX_POPUP_WIDTH,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
        })
    }

    pub fn with_popup_config(mut self, max_visible: usize, min_width: u16, max_width: u16) -> Self {
        self.max_visible = max_visible;
        self.min_width = min_width;
        self.max_width = max_width;
        self
    }

    pub fn with_trigger_chars(mut self, chars: &[char]) -> Self {
        self.trigger_chars = chars.iter().copied().collect();
        self
    }

    pub fn with_keybindings(mut self, keybindings: Keybindings) -> Self {
        self.keybindings = keybindings;
        self
    }

    pub fn with_theme(mut self, theme: PopupTheme) -> Self {
        self.theme = theme;
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_suggest_config(
        self,
        max_results: usize,
        max_history_entries: usize,
        commands: bool,
        history: bool,
        filesystem: bool,
        specs: bool,
        git: bool,
    ) -> Self {
        let engine = self.engine.with_suggest_config(
            max_results,
            max_history_entries,
            commands,
            history,
            filesystem,
            specs,
            git,
        );
        Self { engine, ..self }
    }

    #[allow(dead_code)]
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
        // Configurable actions checked first via if-chain
        if key == &self.keybindings.navigate_up {
            self.overlay.move_up();
            self.render(parser, stdout);
            return Vec::new();
        }
        if key == &self.keybindings.navigate_down {
            self.overlay
                .move_down(self.suggestions.len(), self.max_visible);
            self.render(parser, stdout);
            return Vec::new();
        }
        if key == &self.keybindings.accept {
            if self.overlay.selected.is_none() {
                self.dismiss(stdout);
                return key_to_bytes(key);
            }
            return self.accept_with_chaining(parser, stdout);
        }
        if key == &self.keybindings.accept_and_enter {
            if self.overlay.selected.is_some() {
                let mut forward = self.accept_suggestion(parser);
                self.dismiss(stdout);
                forward.push(0x0D);
                return forward;
            } else {
                self.dismiss(stdout);
                return vec![0x0D];
            }
        }
        if key == &self.keybindings.dismiss {
            self.dismiss(stdout);
            return Vec::new();
        }

        // Structural keys — not configurable
        match key {
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

    /// Accept the current suggestion, with directory chaining for paths ending in '/'.
    fn accept_with_chaining(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) -> Vec<u8> {
        let selected_idx = match self.overlay.selected {
            Some(idx) if idx < self.suggestions.len() => idx,
            _ => {
                self.dismiss(stdout);
                return Vec::new();
            }
        };

        let selected_text = self.suggestions[selected_idx].text.clone();
        let is_dir = selected_text.ends_with('/');
        let forward = self.accept_suggestion(parser);

        if is_dir {
            // CD chaining: predict the buffer after acceptance and
            // immediately show next-level suggestions. Avoids timing
            // issues with the shell's OSC 7770 roundtrip.
            let (cwd, predicted_ctx, cr, cc, sr, sc) = {
                let mut p = parser.lock().unwrap();
                let state = p.state();
                let buffer = state.command_buffer().unwrap_or("").to_string();
                let char_cursor = state.buffer_cursor(); // character offset
                let byte_cursor = char_to_byte_offset(&buffer, char_cursor);
                let old_ctx = parse_command_context(&buffer, char_cursor);
                let word_start_bytes = byte_cursor - old_ctx.current_word.len();

                let mut predicted = String::with_capacity(buffer.len() + selected_text.len());
                predicted.push_str(&buffer[..word_start_bytes]);
                predicted.push_str(&selected_text);
                if byte_cursor < buffer.len() {
                    predicted.push_str(&buffer[byte_cursor..]);
                }
                // new_cursor is a char offset for predict_command_buffer
                let word_start_chars = byte_to_char_offset(&buffer, word_start_bytes);
                let new_cursor = word_start_chars + selected_text.chars().count();

                let cwd = state.cwd().cloned().unwrap_or_else(|| PathBuf::from("."));
                let ctx = parse_command_context(&predicted, new_cursor);
                let (cr, cc) = state.cursor_position();
                let (sr, sc) = state.screen_dimensions();

                // Update parser with predicted buffer so subsequent
                // accept computes correct current_word
                p.state_mut().predict_command_buffer(predicted, new_cursor);

                (cwd, ctx, cr, cc, sr, sc)
            };

            match self.engine.suggest_sync(&predicted_ctx, &cwd) {
                Ok(suggestions) if !suggestions.is_empty() => {
                    self.suggestions = suggestions;
                    self.overlay.reset();
                    self.visible = true;
                    self.render_at(stdout, cr, cc, sr, sc);
                }
                _ => {
                    self.dismiss(stdout);
                }
            }
        } else {
            self.dismiss(stdout);
        }

        forward
    }

    fn process_key_hidden(
        &mut self,
        key: &KeyEvent,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) -> Vec<u8> {
        if key == &self.keybindings.trigger {
            // Manual trigger — fire immediately (no PTY roundtrip needed)
            self.trigger(parser, stdout);
            return Vec::new();
        }
        match key {
            KeyEvent::Printable(c) => {
                let forward = vec![*c as u8];
                if self.trigger_chars.contains(c) {
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
            self.max_visible,
            self.min_width,
            self.max_width,
            &self.theme,
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
        let selected_idx = match self.overlay.selected {
            Some(idx) if idx < self.suggestions.len() => idx,
            _ => return Vec::new(),
        };

        let selected = &self.suggestions[selected_idx];

        let current_word_chars = {
            let p = parser.lock().unwrap();
            let state = p.state();
            let buffer = state.command_buffer().unwrap_or("");
            let cursor = state.buffer_cursor();
            let ctx = parse_command_context(buffer, cursor);
            ctx.current_word.chars().count()
        };

        // One 0x7F (backspace) per CHARACTER — the shell deletes by character, not byte
        let mut bytes = vec![0x7F; current_word_chars];

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

const DEFAULT_TRIGGER_CHARS: &[char] = &[' ', '/', '-', '.'];

#[cfg(test)]
/// Check if a printable character should trigger suggestions (using defaults).
fn should_trigger_on_char(c: char) -> bool {
    DEFAULT_TRIGGER_CHARS.contains(&c)
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
        KeyEvent::CtrlSlash => vec![0x1F],
        KeyEvent::Backspace => vec![0x7F],
        KeyEvent::Printable(c) => vec![*c as u8],
        KeyEvent::CursorPositionReport(_, _) => Vec::new(), // intercepted in proxy
        KeyEvent::Raw(bytes) => bytes.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_overlay::types::{
        DEFAULT_MAX_POPUP_WIDTH, DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH,
    };
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
            KeyEvent::CtrlSlash,
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
            max_visible: DEFAULT_MAX_VISIBLE,
            min_width: DEFAULT_MIN_POPUP_WIDTH,
            max_width: DEFAULT_MAX_POPUP_WIDTH,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
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
            max_visible: DEFAULT_MAX_VISIBLE,
            min_width: DEFAULT_MIN_POPUP_WIDTH,
            max_width: DEFAULT_MAX_POPUP_WIDTH,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
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
        let kb = Keybindings {
            trigger: KeyEvent::CtrlSpace,
            ..Keybindings::default()
        };
        let mut handler = make_handler().with_keybindings(kb);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::CtrlSpace, &parser, &mut buf);
        // CtrlSpace triggers immediately — does NOT set trigger_requested
        assert!(!handler.has_pending_trigger());
    }

    #[test]
    fn test_ctrl_slash_triggers_immediately() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::CtrlSlash, &parser, &mut buf);
        // CtrlSlash is the default trigger — fires immediately
        assert!(!handler.has_pending_trigger());
    }

    #[test]
    fn test_handler_starts_not_visible() {
        let handler = make_handler();
        assert!(!handler.is_visible());
        assert!(!handler.has_pending_trigger());
    }

    #[test]
    fn test_tab_accept_directory_predicts_buffer() {
        let mut handler = InputHandler {
            engine: SuggestionEngine::new(Path::new(".")).unwrap(),
            overlay: OverlayState {
                selected: Some(0),
                scroll_offset: 0,
            },
            suggestions: vec![Suggestion {
                text: "Desktop/".to_string(),
                description: None,
                kind: SuggestionKind::Directory,
                source: SuggestionSource::Filesystem,
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
            max_visible: DEFAULT_MAX_VISIBLE,
            min_width: DEFAULT_MIN_POPUP_WIDTH,
            max_width: DEFAULT_MAX_POPUP_WIDTH,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
        };

        // Simulate buffer "cd " with cursor at 3
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        {
            let mut p = parser.lock().unwrap();
            p.state_mut().predict_command_buffer("cd ".to_string(), 3);
        }

        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::Tab, &parser, &mut buf);

        // Should NOT use deferred trigger — triggers immediately
        assert!(
            !handler.has_pending_trigger(),
            "directory Tab should trigger immediately, not defer"
        );
        // Parser buffer should be updated to predicted state
        {
            let p = parser.lock().unwrap();
            assert_eq!(p.state().command_buffer(), Some("cd Desktop/"));
            assert_eq!(p.state().buffer_cursor(), 11);
        }
    }

    #[test]
    fn test_tab_accept_file_dismisses() {
        let mut handler = InputHandler {
            engine: SuggestionEngine::new(Path::new(".")).unwrap(),
            overlay: OverlayState {
                selected: Some(0),
                scroll_offset: 0,
            },
            suggestions: vec![Suggestion {
                text: "README.md".to_string(),
                description: None,
                kind: SuggestionKind::FilePath,
                source: SuggestionSource::Filesystem,
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
            max_visible: DEFAULT_MAX_VISIBLE,
            min_width: DEFAULT_MIN_POPUP_WIDTH,
            max_width: DEFAULT_MAX_POPUP_WIDTH,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
        };

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::Tab, &parser, &mut buf);
        assert!(
            !handler.visible,
            "popup should dismiss after accepting a file"
        );
    }

    #[test]
    fn test_custom_trigger_chars() {
        let mut handler = make_handler().with_trigger_chars(&['@', '#']);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        // '@' should trigger with custom config
        handler.process_key(&KeyEvent::Printable('@'), &parser, &mut buf);
        assert!(handler.has_pending_trigger());
        handler.clear_trigger_request();

        // Space should NOT trigger with custom config (not in set)
        handler.process_key(&KeyEvent::Printable(' '), &parser, &mut buf);
        assert!(!handler.has_pending_trigger());
    }

    #[test]
    fn test_enter_no_selection_forwards_enter() {
        let mut handler = InputHandler {
            engine: SuggestionEngine::new(Path::new(".")).unwrap(),
            overlay: OverlayState::new(), // selected: None
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
            max_visible: DEFAULT_MAX_VISIBLE,
            min_width: DEFAULT_MIN_POPUP_WIDTH,
            max_width: DEFAULT_MAX_POPUP_WIDTH,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
        };

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        let result = handler.process_key(&KeyEvent::Enter, &parser, &mut buf);

        assert_eq!(
            result,
            vec![0x0D],
            "should forward Enter when nothing selected"
        );
        assert!(!handler.visible, "popup should be dismissed");
    }

    #[test]
    fn test_tab_no_selection_forwards_tab() {
        let mut handler = InputHandler {
            engine: SuggestionEngine::new(Path::new(".")).unwrap(),
            overlay: OverlayState::new(), // selected: None
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
            max_visible: DEFAULT_MAX_VISIBLE,
            min_width: DEFAULT_MIN_POPUP_WIDTH,
            max_width: DEFAULT_MAX_POPUP_WIDTH,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
        };

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        let result = handler.process_key(&KeyEvent::Tab, &parser, &mut buf);

        assert_eq!(
            result,
            vec![0x09],
            "should forward Tab when nothing selected"
        );
        assert!(!handler.visible, "popup should be dismissed");
    }

    // --- parse_key_name tests ---

    #[test]
    fn test_parse_key_name_all_supported() {
        assert_eq!(parse_key_name("tab").unwrap(), KeyEvent::Tab);
        assert_eq!(parse_key_name("enter").unwrap(), KeyEvent::Enter);
        assert_eq!(parse_key_name("escape").unwrap(), KeyEvent::Escape);
        assert_eq!(parse_key_name("backspace").unwrap(), KeyEvent::Backspace);
        assert_eq!(parse_key_name("ctrl+space").unwrap(), KeyEvent::CtrlSpace);
        assert_eq!(parse_key_name("ctrl+/").unwrap(), KeyEvent::CtrlSlash);
        assert_eq!(parse_key_name("arrow_up").unwrap(), KeyEvent::ArrowUp);
        assert_eq!(parse_key_name("arrow_down").unwrap(), KeyEvent::ArrowDown);
        assert_eq!(parse_key_name("arrow_left").unwrap(), KeyEvent::ArrowLeft);
        assert_eq!(parse_key_name("arrow_right").unwrap(), KeyEvent::ArrowRight);
    }

    #[test]
    fn test_parse_key_name_case_insensitive() {
        assert_eq!(parse_key_name("Tab").unwrap(), KeyEvent::Tab);
        assert_eq!(parse_key_name("TAB").unwrap(), KeyEvent::Tab);
        assert_eq!(parse_key_name("CTRL+SPACE").unwrap(), KeyEvent::CtrlSpace);
        assert_eq!(parse_key_name("CTRL+/").unwrap(), KeyEvent::CtrlSlash);
        assert_eq!(parse_key_name("Arrow_Up").unwrap(), KeyEvent::ArrowUp);
        assert_eq!(parse_key_name("ESCAPE").unwrap(), KeyEvent::Escape);
    }

    #[test]
    fn test_parse_key_name_trims_whitespace() {
        assert_eq!(parse_key_name("  tab  ").unwrap(), KeyEvent::Tab);
        assert_eq!(parse_key_name(" ctrl+space ").unwrap(), KeyEvent::CtrlSpace);
    }

    #[test]
    fn test_parse_key_name_unknown_errors() {
        assert!(parse_key_name("f1").is_err());
        assert!(parse_key_name("ctrl+c").is_err());
        assert!(parse_key_name("").is_err());
        assert!(parse_key_name("banana").is_err());
    }

    // --- Keybindings tests ---

    #[test]
    fn test_keybindings_from_default_config() {
        let config = gc_config::KeybindingsConfig::default();
        let kb = Keybindings::from_config(&config).unwrap();
        assert_eq!(kb, Keybindings::default());
    }

    #[test]
    fn test_keybindings_from_custom_config() {
        let config = gc_config::KeybindingsConfig {
            accept: "enter".to_string(),
            accept_and_enter: "tab".to_string(),
            dismiss: "backspace".to_string(),
            navigate_up: "ctrl+space".to_string(),
            navigate_down: "arrow_right".to_string(),
            trigger: "tab".to_string(),
        };
        let kb = Keybindings::from_config(&config).unwrap();
        assert_eq!(kb.accept, KeyEvent::Enter);
        assert_eq!(kb.accept_and_enter, KeyEvent::Tab);
        assert_eq!(kb.dismiss, KeyEvent::Backspace);
        assert_eq!(kb.navigate_up, KeyEvent::CtrlSpace);
        assert_eq!(kb.navigate_down, KeyEvent::ArrowRight);
        assert_eq!(kb.trigger, KeyEvent::Tab);
    }

    #[test]
    fn test_keybindings_from_config_invalid_key() {
        let config = gc_config::KeybindingsConfig {
            accept: "nonexistent".to_string(),
            ..gc_config::KeybindingsConfig::default()
        };
        assert!(Keybindings::from_config(&config).is_err());
    }

    // --- Custom keybinding behavior test ---

    #[test]
    fn test_custom_keybinding_trigger() {
        let kb = Keybindings {
            trigger: KeyEvent::Tab, // Tab triggers instead of Ctrl+Space
            ..Keybindings::default()
        };
        let mut handler = make_handler().with_keybindings(kb);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        // Tab should now act as trigger when popup is hidden
        handler.process_key(&KeyEvent::Tab, &parser, &mut buf);
        // Tab triggers immediately (like CtrlSpace normally does)
        assert!(!handler.has_pending_trigger());

        // CtrlSpace should pass through as raw bytes since it's no longer trigger
        let result = handler.process_key(&KeyEvent::CtrlSpace, &parser, &mut buf);
        assert_eq!(result, vec![0x00]);
    }
}
