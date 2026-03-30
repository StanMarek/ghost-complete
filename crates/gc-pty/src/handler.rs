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
use gc_terminal::TerminalProfile;
use tokio::sync::{mpsc, Notify};

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
    engine: Arc<SuggestionEngine>,
    overlay: OverlayState,
    suggestions: Vec<Suggestion>,
    last_layout: Option<PopupLayout>,
    visible: bool,
    trigger_requested: bool,
    max_visible: usize,
    trigger_chars: HashSet<char>,
    debounce_suppressed: bool,
    keybindings: Keybindings,
    theme: PopupTheme,
    dynamic_rx: Option<mpsc::Receiver<Vec<Suggestion>>>,
    dynamic_notify: Arc<Notify>,
    terminal_profile: TerminalProfile,
    /// Accumulated viewport scroll from popup rendering. Persists across
    /// dismiss/re-trigger cycles because viewport scroll is permanent.
    /// Reset when a CPR sync corrects the parser's cursor position (new prompt).
    scroll_deficit: u16,
}

impl InputHandler {
    pub fn new(spec_dir: &Path, terminal_profile: TerminalProfile) -> anyhow::Result<Self> {
        Ok(Self {
            engine: Arc::new(SuggestionEngine::new(spec_dir)?),
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
            trigger_requested: false,
            max_visible: DEFAULT_MAX_VISIBLE,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            debounce_suppressed: false,
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
            dynamic_rx: None,
            dynamic_notify: Arc::new(Notify::new()),
            terminal_profile,
            scroll_deficit: 0,
        })
    }

    pub fn dynamic_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.dynamic_notify)
    }

    pub fn with_popup_config(mut self, max_visible: usize) -> Self {
        self.max_visible = max_visible;
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
        commands: bool,
        max_history_results: usize,
        filesystem: bool,
        specs: bool,
        git: bool,
    ) -> Self {
        // During builder phase the Arc has exactly one reference, so try_unwrap succeeds.
        let engine = Arc::try_unwrap(self.engine)
            .unwrap_or_else(|_| panic!("with_suggest_config called after engine was shared"))
            .with_suggest_config(
                max_results,
                commands,
                max_history_results,
                filesystem,
                specs,
                git,
            );
        Self {
            engine: Arc::new(engine),
            ..self
        }
    }

    /// Update runtime-configurable fields without restarting the proxy.
    /// Called by the config file watcher when config.toml changes on disk.
    pub fn update_config(
        &mut self,
        theme: PopupTheme,
        keybindings: Keybindings,
        trigger_chars: &[char],
        max_visible: usize,
    ) {
        self.theme = theme;
        self.keybindings = keybindings;
        self.trigger_chars = trigger_chars.iter().copied().collect();
        self.max_visible = max_visible;
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

    pub fn is_debounce_suppressed(&self) -> bool {
        self.debounce_suppressed
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
        let selected_kind = self.suggestions[selected_idx].kind;
        let is_dir = selected_text.ends_with('/');
        let forward = self.accept_suggestion(parser);

        // History entries never chain — they're full commands, not directory paths
        if selected_kind == gc_suggest::SuggestionKind::History {
            self.dismiss(stdout);
            return forward;
        }

        if is_dir {
            // CD chaining: predict the buffer after acceptance and
            // immediately show next-level suggestions. Avoids timing
            // issues with the shell's OSC 7770 roundtrip.
            let (cwd, predicted_ctx, predicted_buffer, cr, cc, sr, sc) = {
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

                let predicted_buf = predicted.clone();

                // Update parser with predicted buffer so subsequent
                // accept computes correct current_word
                p.state_mut().predict_command_buffer(predicted, new_cursor);

                (cwd, ctx, predicted_buf, cr, cc, sr, sc)
            };

            match self
                .engine
                .suggest_sync(&predicted_ctx, &cwd, &predicted_buffer)
            {
                Ok(result) if !result.suggestions.is_empty() => {
                    self.suggestions = result.suggestions;
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
            // Append trailing space so the user can immediately type the next
            // argument. Skip for text ending in '=' (flag expecting value like
            // --output=) since the user needs to type the value directly.
            if !selected_text.ends_with('=') {
                return [forward, vec![b' ']].concat();
            }
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
            self.debounce_suppressed = false;
            self.trigger(parser, stdout);
            return Vec::new();
        }
        match key {
            KeyEvent::Printable(c) => {
                self.debounce_suppressed = false;
                let forward = vec![*c as u8];
                if self.trigger_chars.contains(c) {
                    // Defer trigger to Task B after shell processes the keystroke
                    self.trigger_requested = true;
                }
                forward
            }
            KeyEvent::ArrowUp | KeyEvent::ArrowDown => {
                // History navigation — suppress debounce so the popup doesn't
                // trigger on buffer changes from shell history recall.
                self.debounce_suppressed = true;
                key_to_bytes(key)
            }
            _ => key_to_bytes(key),
        }
    }

    pub fn trigger(&mut self, parser: &Arc<Mutex<TerminalParser>>, stdout: &mut dyn Write) {
        let (buffer, cursor, cwd, cursor_row, cursor_col, screen_rows, screen_cols) = {
            let mut p = parser.lock().unwrap();
            // CPR sync means the parser's cursor_row now reflects reality,
            // so any accumulated scroll deficit from prior renders is stale.
            if p.state_mut().take_cpr_synced() {
                self.scroll_deficit = 0;
            }
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

        // Drop any pending dynamic results from a previous trigger
        self.dynamic_rx = None;

        let sync_result = self.engine.suggest_sync(&ctx, &cwd, &buffer);

        match sync_result {
            Ok(result) if !result.suggestions.is_empty() => {
                self.suggestions = result.suggestions;
                self.overlay.reset();
                self.visible = true;
                self.render_at(stdout, cursor_row, cursor_col, screen_rows, screen_cols);
                self.spawn_generators(result.script_generators, &ctx, &cwd);
            }
            Ok(result) => {
                if !result.script_generators.is_empty() {
                    self.spawn_generators(result.script_generators, &ctx, &cwd);
                } else if self.visible {
                    self.dismiss(stdout);
                }
            }
            Err(e) => {
                tracing::debug!("suggest_sync failed: {e}");
                if self.visible {
                    self.dismiss(stdout);
                }
            }
        }
    }

    /// Spawn an async task to run pre-resolved script generators. Results
    /// arrive via `dynamic_rx` and Task E renders them via `dynamic_notify`.
    fn spawn_generators(
        &mut self,
        generators: Vec<gc_suggest::specs::GeneratorSpec>,
        ctx: &gc_buffer::CommandContext,
        cwd: &std::path::Path,
    ) {
        if generators.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel::<Vec<Suggestion>>(1);
        self.dynamic_rx = Some(rx);
        let engine = Arc::clone(&self.engine);
        let ctx = ctx.clone();
        let cwd = cwd.to_path_buf();
        let timeout = GENERATOR_TIMEOUT_MS;
        let notify = Arc::clone(&self.dynamic_notify);
        tokio::spawn(async move {
            match engine
                .run_generators(&generators, &ctx, &cwd, timeout)
                .await
            {
                Ok(results) if !results.is_empty() => {
                    let _ = tx.send(results).await;
                }
                Ok(_) => {} // empty results — tx dropped, channel disconnects
                Err(e) => {
                    tracing::debug!("dynamic suggestions failed: {e}");
                }
            }
            // Always notify so Task E clears the loading indicator,
            // even when generators returned empty or errored.
            notify.notify_one();
        });
    }

    /// Check for pending dynamic (script generator) results and merge them
    /// into the current suggestions. Returns `true` if the popup was updated.
    pub fn try_merge_dynamic(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) -> bool {
        let rx = match self.dynamic_rx.as_mut() {
            Some(rx) => rx,
            None => return false,
        };

        match rx.try_recv() {
            Ok(dynamic_results) => {
                self.dynamic_rx = None;
                if !self.visible || dynamic_results.is_empty() {
                    return false;
                }

                // Merge: add dynamic results, dedup by text
                let mut seen: HashSet<String> =
                    self.suggestions.iter().map(|s| s.text.clone()).collect();
                for s in dynamic_results {
                    if seen.insert(s.text.clone()) {
                        self.suggestions.push(s);
                    }
                }

                // Re-rank against the current query — the user may have
                // typed more characters while generators were running.
                let current_word = {
                    let p = match parser.lock() {
                        Ok(p) => p,
                        Err(_) => {
                            self.render(parser, stdout);
                            return true;
                        }
                    };
                    let state = p.state();
                    let buffer = state.command_buffer().unwrap_or("");
                    let cursor = state.buffer_cursor();
                    let ctx = parse_command_context(buffer, cursor);
                    ctx.current_word
                };
                let merged = std::mem::take(&mut self.suggestions);
                self.suggestions =
                    gc_suggest::fuzzy::rank(&current_word, merged, self.max_visible * 5);

                if self.suggestions.is_empty() {
                    self.dismiss(stdout);
                    return true;
                }

                self.render(parser, stdout);
                true
            }
            Err(mpsc::error::TryRecvError::Empty) => false,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.dynamic_rx = None;
                false
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
        // For PreRenderBuffer strategy, we must combine clear + render into a
        // single buffer and emit one write() call for flicker-free atomicity.
        // For Synchronized strategy, DECSET 2026 markers handle this at the
        // terminal level so separate writes are fine.
        let mut buf = Vec::new();

        if let Some(ref layout) = self.last_layout {
            clear_popup(&mut buf, layout, &self.terminal_profile);
        }

        let loading = self.dynamic_rx.is_some();
        let layout = render_popup(
            &mut buf,
            &self.suggestions,
            &self.overlay,
            cursor_row,
            cursor_col,
            screen_rows,
            screen_cols,
            self.max_visible,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &self.theme,
            self.scroll_deficit,
            loading,
            &self.terminal_profile,
        );
        let _ = stdout.write_all(&buf);
        let _ = stdout.flush();
        self.scroll_deficit = layout.scroll_deficit;
        self.last_layout = Some(layout);
    }

    fn dismiss(&mut self, stdout: &mut dyn Write) {
        if let Some(ref layout) = self.last_layout {
            let mut buf = Vec::new();
            clear_popup(&mut buf, layout, &self.terminal_profile);
            let _ = stdout.write_all(&buf);
            let _ = stdout.flush();
        }
        self.visible = false;
        self.suggestions.clear();
        self.overlay.reset();
        self.last_layout = None;
        self.dynamic_rx = None;
    }

    fn accept_suggestion(&self, parser: &Arc<Mutex<TerminalParser>>) -> Vec<u8> {
        let selected_idx = match self.overlay.selected {
            Some(idx) if idx < self.suggestions.len() => idx,
            _ => return Vec::new(),
        };

        let selected = &self.suggestions[selected_idx];

        let (delete_chars, replacement) = {
            let p = parser.lock().unwrap();
            let state = p.state();
            let buffer = state.command_buffer().unwrap_or("");
            let cursor = state.buffer_cursor();

            if selected.kind == gc_suggest::SuggestionKind::History {
                // History: delete the entire buffer up to cursor, then type the full command.
                // Cursor is always at buffer end when popup is visible (arrow keys dismiss),
                // but we use cursor (not buffer.chars().count()) because over-deleting past
                // cursor into the prompt would be worse than leaving trailing chars.
                debug_assert_eq!(
                    cursor,
                    buffer.chars().count(),
                    "history accept assumes cursor at end of buffer"
                );
                (cursor, selected.text.clone())
            } else {
                // Non-history: delete current_word, type suggestion text
                let ctx = parse_command_context(buffer, cursor);
                (ctx.current_word.chars().count(), selected.text.clone())
            }
        };

        // One 0x7F (backspace) per CHARACTER — the shell deletes by character, not byte
        let mut bytes = vec![0x7F; delete_chars];
        bytes.extend_from_slice(replacement.as_bytes());

        bytes
    }

    /// Handle terminal resize while popup is visible.
    /// Dismisses popup instead of re-rendering — after a resize, screen dimensions
    /// change and prior scroll deficit is stale. Popup recomputes on next trigger.
    pub fn handle_resize(&mut self, _parser: &Arc<Mutex<TerminalParser>>, stdout: &mut dyn Write) {
        if self.visible {
            self.dismiss(stdout);
        }
        // Screen dimensions changed — prior scroll deficit is meaningless.
        self.scroll_deficit = 0;
    }
}

const DEFAULT_TRIGGER_CHARS: &[char] = &[' ', '/', '-', '.'];
const GENERATOR_TIMEOUT_MS: u64 = 5000;

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
    use gc_overlay::types::DEFAULT_MAX_VISIBLE;
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
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "test".to_string(),
            ..Default::default()
        }]);

        let mut stdout_buf = Vec::new();
        handler.dismiss(&mut stdout_buf);

        assert!(!handler.visible);
        assert!(handler.suggestions.is_empty());
        assert!(handler.last_layout.is_none());
        assert!(!stdout_buf.is_empty());
    }

    fn make_handler() -> InputHandler {
        InputHandler {
            engine: Arc::new(SuggestionEngine::new(Path::new(".")).unwrap()),
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
            trigger_requested: false,
            max_visible: DEFAULT_MAX_VISIBLE,
            trigger_chars: DEFAULT_TRIGGER_CHARS.iter().copied().collect(),
            debounce_suppressed: false,
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
            dynamic_rx: None,
            dynamic_notify: Arc::new(Notify::new()),
            terminal_profile: TerminalProfile::for_ghostty(),
            scroll_deficit: 0,
        }
    }

    /// Test builder: set up a visible popup with suggestions and a default layout.
    fn make_visible_handler(suggestions: Vec<Suggestion>) -> InputHandler {
        let mut h = make_handler();
        h.suggestions = suggestions;
        h.visible = true;
        h.last_layout = Some(PopupLayout {
            start_row: 5,
            start_col: 0,
            width: 20,
            height: 1,
            scroll_deficit: 0,
        });
        h
    }

    /// Test builder: visible handler with a single selected suggestion.
    fn make_selected_handler(suggestion: Suggestion) -> InputHandler {
        let mut h = make_visible_handler(vec![suggestion]);
        h.overlay.selected = Some(0);
        h
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
        let mut handler = make_selected_handler(Suggestion {
            text: "Desktop/".to_string(),
            kind: SuggestionKind::Directory,
            source: SuggestionSource::Filesystem,
            ..Default::default()
        });

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
        let mut handler = make_selected_handler(Suggestion {
            text: "README.md".to_string(),
            kind: SuggestionKind::FilePath,
            source: SuggestionSource::Filesystem,
            ..Default::default()
        });

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        let result = handler.process_key(&KeyEvent::Tab, &parser, &mut buf);
        assert!(
            !handler.visible,
            "popup should dismiss after accepting a file"
        );
        assert!(
            result.ends_with(b" "),
            "accepting a file should append trailing space, got: {result:?}"
        );
    }

    #[test]
    fn test_tab_accept_flag_ending_with_equals_no_space() {
        let mut handler = make_selected_handler(Suggestion {
            text: "--output=".to_string(),
            kind: SuggestionKind::Flag,
            source: SuggestionSource::Spec,
            ..Default::default()
        });

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        let result = handler.process_key(&KeyEvent::Tab, &parser, &mut buf);
        assert!(
            !result.ends_with(b" "),
            "flags ending with = should NOT get trailing space, got: {result:?}"
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
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "test".to_string(),
            ..Default::default()
        }]);

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
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "test".to_string(),
            ..Default::default()
        }]);

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

    // --- update_config tests ---

    #[test]
    fn test_update_config_changes_theme() {
        let mut handler = make_handler();
        // Default theme uses \x1b[7m for selected (reverse video)
        assert_eq!(handler.theme.selected_on, b"\x1b[7m".to_vec());

        let new_theme = PopupTheme {
            selected_on: vec![0x1B, b'[', b'1', b'm'],
            description_on: vec![0x1B, b'[', b'2', b'm'],
            match_highlight_on: vec![0x1B, b'[', b'4', b'm'],
            item_text_on: vec![],
            scrollbar_on: vec![0x1B, b'[', b'2', b'm'],
        };

        handler.update_config(new_theme, Keybindings::default(), &[' ', '/'], 15);

        assert_eq!(handler.theme.selected_on, vec![0x1B, b'[', b'1', b'm']);
        assert_eq!(handler.theme.description_on, vec![0x1B, b'[', b'2', b'm']);
    }

    #[test]
    fn test_update_config_changes_keybindings() {
        let mut handler = make_handler();

        let new_kb = Keybindings {
            accept: KeyEvent::Enter,
            accept_and_enter: KeyEvent::Tab,
            dismiss: KeyEvent::Backspace,
            navigate_up: KeyEvent::CtrlSpace,
            navigate_down: KeyEvent::ArrowRight,
            trigger: KeyEvent::Tab,
        };

        handler.update_config(PopupTheme::default(), new_kb.clone(), &[' ', '/'], 10);

        assert_eq!(handler.keybindings, new_kb);
    }

    #[test]
    fn test_update_config_changes_max_visible() {
        let mut handler = make_handler();

        handler.update_config(
            PopupTheme::default(),
            Keybindings::default(),
            &['@', '#'],
            20,
        );

        assert_eq!(handler.max_visible, 20);
    }

    #[test]
    fn test_update_config_changes_trigger_chars() {
        let mut handler = make_handler();

        handler.update_config(
            PopupTheme::default(),
            Keybindings::default(),
            &['@', '#', '!'],
            10,
        );

        let expected: HashSet<char> = ['@', '#', '!'].iter().copied().collect();
        assert_eq!(handler.trigger_chars, expected);
    }

    // --- Debounce suppression tests ---

    #[test]
    fn test_arrow_up_suppresses_debounce() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        assert!(!handler.is_debounce_suppressed());
        handler.process_key(&KeyEvent::ArrowUp, &parser, &mut buf);
        assert!(handler.is_debounce_suppressed());
    }

    #[test]
    fn test_arrow_down_suppresses_debounce() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::ArrowDown, &parser, &mut buf);
        assert!(handler.is_debounce_suppressed());
    }

    #[test]
    fn test_printable_clears_debounce_suppression() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        // Suppress via arrow
        handler.process_key(&KeyEvent::ArrowUp, &parser, &mut buf);
        assert!(handler.is_debounce_suppressed());
        // Clear via typing
        handler.process_key(&KeyEvent::Printable('a'), &parser, &mut buf);
        assert!(!handler.is_debounce_suppressed());
    }

    #[test]
    fn test_manual_trigger_clears_debounce_suppression() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        // Suppress via arrow
        handler.process_key(&KeyEvent::ArrowUp, &parser, &mut buf);
        assert!(handler.is_debounce_suppressed());
        // Clear via manual trigger (Ctrl+/)
        handler.process_key(&KeyEvent::CtrlSlash, &parser, &mut buf);
        assert!(!handler.is_debounce_suppressed());
    }
}
