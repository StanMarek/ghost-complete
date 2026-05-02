use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gc_buffer::{byte_to_char_offset, char_to_byte_offset, parse_command_context};
use gc_overlay::types::{
    OverlayState, PopupLayout, DEFAULT_MAX_POPUP_WIDTH, DEFAULT_MAX_VISIBLE,
    DEFAULT_MIN_POPUP_WIDTH,
};
use gc_overlay::{clear_popup, render_indicator_row, render_popup, FeedbackKind, PopupTheme};
use gc_parser::TerminalParser;
use gc_suggest::{Suggestion, SuggestionEngine};
use gc_terminal::TerminalProfile;
use tokio::sync::{mpsc, Notify};

use crate::dynamic_result::{DynamicResult, ProviderTag};
use crate::feedback::AsyncFeedback;
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
/// `arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`, `ctrl+a`-`ctrl+z`
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
        other => {
            if let Some(c) = other.strip_prefix("ctrl+") {
                if let Some(ch) = c.chars().next() {
                    if c.len() == 1 && ch.is_ascii_lowercase() {
                        match ch {
                            'c' => anyhow::bail!("ctrl+c is reserved for SIGINT — cannot be used as a keybinding"),
                            'd' => anyhow::bail!("ctrl+d is reserved for EOF — cannot be used as a keybinding"),
                            'z' => anyhow::bail!("ctrl+z is reserved for SIGTSTP — cannot be used as a keybinding"),
                            's' => anyhow::bail!("ctrl+s is reserved for flow control (XOFF) — cannot be used as a keybinding"),
                            'q' => anyhow::bail!("ctrl+q is reserved for flow control (XON) — cannot be used as a keybinding"),
                            'i' => anyhow::bail!("ctrl+i produces the same byte as Tab (0x09) — use 'tab' instead"),
                            'm' => anyhow::bail!("ctrl+m produces the same byte as Enter (0x0D) — use 'enter' instead"),
                            _ => return Ok(KeyEvent::Ctrl(ch)),
                        }
                    }
                }
                anyhow::bail!(
                    "ctrl+ must be followed by a single letter (a-z), got: {:?}",
                    c
                );
            }
            anyhow::bail!("unknown key name: {:?}", other)
        }
    }
}

/// Snapshot of command context at generator-spawn time, so merge-time can
/// decide whether in-flight results still match the user's current buffer.
/// `spawned_current_word` is `Some` only for generators that embed the word
/// literally (e.g. script templates with `{current_token}`).
#[derive(Debug, Clone)]
struct DynamicCtxSnapshot {
    command: Option<String>,
    args: Vec<String>,
    preceding_flag: Option<String>,
    word_index: usize,
    /// The `current_word` at spawn time, but ONLY when a generator depends
    /// on its value. `None` for generators that treat `current_word` purely
    /// as a fuzzy-filter prefix (git branches, plain scripts, filesystem).
    spawned_current_word: Option<String>,
}

impl DynamicCtxSnapshot {
    fn capture(ctx: &gc_buffer::CommandContext, uses_current_word: bool) -> Self {
        Self {
            command: ctx.command.clone(),
            args: ctx.args.clone(),
            preceding_flag: ctx.preceding_flag.clone(),
            word_index: ctx.word_index,
            spawned_current_word: if uses_current_word {
                Some(ctx.current_word.clone())
            } else {
                None
            },
        }
    }

    /// Returns true if `current` represents a different completion site than
    /// the site this snapshot was taken at — in which case in-flight results
    /// are stale and must not be merged.
    fn is_stale_against(&self, current: &gc_buffer::CommandContext) -> bool {
        if self.command != current.command
            || self.args != current.args
            || self.preceding_flag != current.preceding_flag
            || self.word_index != current.word_index
        {
            return true;
        }
        // `script_template` generators (the only case where this field is
        // Some) substitute `{current_token}` LITERALLY into the generator's
        // command line, per docs/COMPLETION_SPEC.md. `docker inspect ar` and
        // `docker inspect arg` are independent commands producing disjoint
        // result sets — prefix extension is unsound because results are not
        // a superset/subset relationship. Any change to current_word means
        // the results are from a different command invocation entirely.
        if let Some(ref spawned_word) = self.spawned_current_word {
            if spawned_word != &current.current_word {
                return true;
            }
        }
        false
    }
}

/// Outcome of the staleness check shared by `try_merge_dynamic` and
/// `apply_block_result`.
enum MergeFreshness {
    /// Spawn-time context still matches the live buffer; merge with the
    /// returned live `current_word`.
    Fresh { current_word: String },
    /// Buffer drifted — drop the results and repaint.
    Stale,
    /// Parser lock was poisoned; caller should bail without rendering.
    PoisonedLock,
}

/// Result of `InputHandler::prepare_trigger_with_block`.
///
/// When a high-priority async generator is pending and `render_block_ms > 0`,
/// the debounce loop receives `NeedsBlock` and awaits the bounded window
/// *outside* the `std::sync::Mutex` lock so Tokio can schedule other tasks
/// during the wait. On timeout or fast-completion the loop re-acquires the
/// lock to call `apply_block_result`.
pub enum TriggerPrepared {
    /// Sync-only suggestions were painted (or the trigger was a no-op). No
    /// further action needed from the caller.
    Painted,
    /// Sync suggestions were painted and a high-priority async generator is
    /// pending. The caller should await `rx` up to `block_ms`, then call
    /// `apply_block_result` under the handler lock.
    NeedsBlock {
        /// Receiver to `await` for the async generator's results.
        rx: mpsc::Receiver<DynamicResult>,
        /// Sync-only suggestions already painted. Used for merging.
        sync_suggestions: Vec<Suggestion>,
        /// Maximum wait duration.
        block_ms: u64,
        /// Cursor geometry for the follow-up render.
        cursor_row: u16,
        cursor_col: u16,
        screen_rows: u16,
        screen_cols: u16,
        /// Fingerprint to stamp on `last_trigger_fingerprint` after a
        /// successful merge render.
        fingerprint: (u64, usize),
        /// Current word at trigger time. Passed to `apply_block_result` so
        /// the merge step filters/ranks the combined pool against the user's
        /// query (mirrors the empty-vs-non-empty branch in `try_merge_dynamic`).
        current_word: String,
    },
}

pub struct InputHandler {
    engine: Arc<SuggestionEngine>,
    overlay: OverlayState,
    suggestions: Vec<Suggestion>,
    last_layout: Option<PopupLayout>,
    visible: bool,
    trigger_requested: bool,
    max_visible: usize,
    // Small Vec<char>: at ~4-element cardinality a linear scan beats hashing.
    trigger_chars: Vec<char>,
    debounce_suppressed: bool,
    auto_trigger: bool,
    keybindings: Keybindings,
    theme: PopupTheme,
    /// Per-spawn timeout (ms) applied to async script / git generators.
    /// Populated via [`InputHandler::with_suggest_config`] during builder
    /// phase, defaulting to [`DEFAULT_GENERATOR_TIMEOUT_MS`] when unset.
    generator_timeout_ms: u64,
    dynamic_rx: Option<mpsc::Receiver<DynamicResult>>,
    dynamic_task: Option<tokio::task::JoinHandle<()>>,
    dynamic_notify: Arc<Notify>,
    feedback_tick_notify: Arc<Notify>,
    feedback: AsyncFeedback,
    feedback_dismiss_ms: u16,
    pending_failed: Vec<String>,
    pending_empty_count: usize,
    /// Command context snapshot captured when generators were spawned.
    /// Consulted by `check_merge_staleness` (called from both
    /// `try_merge_dynamic` and `apply_block_result`) to drop stale results
    /// when the user's editing has changed WHICH generator would now apply.
    /// We compare command + args (subcommand path) + preceding_flag +
    /// word_index. `current_word` is also compared, but ONLY when a generator
    /// depends on it literally (script_template with `{current_token}`); for
    /// generators that treat it as a fuzzy-filter prefix, typing more
    /// characters still lets results merge and re-rank.
    /// See `DynamicCtxSnapshot::capture` and `is_stale_against`.
    dynamic_ctx: Option<DynamicCtxSnapshot>,
    terminal_profile: TerminalProfile,
    /// Accumulated viewport scroll from popup rendering. Persists across
    /// dismiss/re-trigger cycles because viewport scroll is permanent.
    /// Reset when a CPR sync corrects the parser's cursor position (new prompt).
    scroll_deficit: u16,
    /// Fingerprint (buffer hash + cursor offset) of the last trigger that
    /// produced a visible popup. Used as an idempotency guard in the trigger
    /// paths (`InputHandler::trigger` and `prepare_trigger_with_block`/
    /// `apply_block_result`): when a new trigger arrives with an unchanged
    /// buffer AND the popup is still visible, we skip re-running
    /// `suggest_sync` because (1) it would produce the same suggestions —
    /// wasted work, and (2) an empty-sync + no-generators result would
    /// silently dismiss a popup populated by a prior trigger's async merge.
    /// See the bug-repro test `test_trigger_idempotent_when_buffer_unchanged`.
    /// Reset by `dismiss()` so ESC-then-retrigger on the same buffer still works.
    last_trigger_fingerprint: Option<(u64, usize)>,
    /// Monotonic counter ticked by both `trigger()` and
    /// `prepare_trigger_with_block()` before the new sync pass runs.
    /// `spawn_generators` snapshots it into `spawned_generation`;
    /// `check_merge_staleness` compares the two so async results spawned for
    /// an earlier buffer get dropped instead of merged.
    buffer_generation: u64,
    /// Generation counter snapshotted at `spawn_generators` time.
    /// `try_merge_dynamic` compares this against `buffer_generation` to drop
    /// results from a task spawned for an earlier buffer state.
    spawned_generation: u64,
    /// Maximum time (ms) to wait for a high-priority async generator before
    /// painting sync-only results. 0 disables bounded blocking (paint immediately).
    /// Set from `config.popup.render_block_ms` during the builder phase.
    render_block_ms: u64,
}

impl InputHandler {
    pub fn new(spec_dirs: &[PathBuf], terminal_profile: TerminalProfile) -> anyhow::Result<Self> {
        Ok(Self {
            engine: Arc::new(SuggestionEngine::new(spec_dirs)?),
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
            trigger_requested: false,
            max_visible: DEFAULT_MAX_VISIBLE,
            trigger_chars: DEFAULT_TRIGGER_CHARS.to_vec(),
            debounce_suppressed: false,
            auto_trigger: true,
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
            generator_timeout_ms: DEFAULT_GENERATOR_TIMEOUT_MS,
            dynamic_rx: None,
            dynamic_task: None,
            dynamic_notify: Arc::new(Notify::new()),
            feedback_tick_notify: Arc::new(Notify::new()),
            feedback: AsyncFeedback::Idle,
            feedback_dismiss_ms: 1200,
            pending_failed: Vec::new(),
            pending_empty_count: 0,
            dynamic_ctx: None,
            terminal_profile,
            scroll_deficit: 0,
            last_trigger_fingerprint: None,
            buffer_generation: 0,
            spawned_generation: 0,
            render_block_ms: 80,
        })
    }

    pub fn dynamic_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.dynamic_notify)
    }

    pub fn feedback_tick_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.feedback_tick_notify)
    }

    pub fn with_popup_config(mut self, max_visible: usize) -> Self {
        self.max_visible = max_visible;
        self
    }

    pub fn with_feedback_dismiss_ms(mut self, ms: u16) -> Self {
        self.feedback_dismiss_ms = ms;
        self
    }

    /// Set the maximum time (ms) to block waiting for a high-priority async
    /// generator before painting sync-only results. 0 disables bounded
    /// blocking. Corresponds to `config.popup.render_block_ms`.
    pub fn with_render_block_ms(mut self, ms: u64) -> Self {
        self.render_block_ms = ms;
        self
    }

    pub fn with_trigger_chars(mut self, chars: &[char]) -> Self {
        self.trigger_chars = chars.to_vec();
        self
    }

    pub fn with_auto_trigger(mut self, auto_trigger: bool) -> Self {
        self.auto_trigger = auto_trigger;
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

    /// Apply suggestion engine configuration during the builder phase.
    ///
    /// # Contract
    ///
    /// - **Must be called before the handler is shared.** Internally this
    ///   `try_unwrap`s the engine `Arc`, which only succeeds while the
    ///   refcount is exactly 1. Once the handler has been wrapped in
    ///   `Arc<Mutex<InputHandler>>` and handed off to the proxy tasks
    ///   (see `proxy.rs`), calling this method will panic with
    ///   `"with_suggest_config called after engine was shared"`.
    /// - **Builder phase only.** Call site convention is a single chained
    ///   `.with_suggest_config(...)` on the freshly constructed handler,
    ///   before any `handle_*` / `process_key` call.
    /// - **If never called**, the engine uses whatever defaults
    ///   `SuggestionEngine::new()` installs (all providers on,
    ///   `DEFAULT_MAX_RESULTS` for both main and history pools) and
    ///   `generator_timeout_ms` stays at [`DEFAULT_GENERATOR_TIMEOUT_MS`].
    /// - **Eager vs. lazy fields.** The provider / result-cap parameters are
    ///   baked into the engine at construction; `generator_timeout_ms` is
    ///   stored on the handler and read on every `spawn_generators` call.
    ///   None of them change thereafter without going through
    ///   [`InputHandler::update_config`] / a runtime reload path.
    /// - **Repeated calls** are supported in theory (each call consumes
    ///   `self` and rebuilds the engine) but the current call path in
    ///   `proxy.rs` only invokes it once, so treat it as idempotent-by-replacement.
    #[allow(clippy::too_many_arguments)]
    pub fn with_suggest_config(
        self,
        max_results: usize,
        commands: bool,
        max_history_results: usize,
        filesystem: bool,
        specs: bool,
        git: bool,
        generator_timeout_ms: u64,
    ) -> Self {
        // During builder phase the Arc has exactly one reference, so try_unwrap succeeds.
        // Can't use .expect() directly because SuggestionEngine doesn't derive Debug;
        // unwrap_or_else with an explicit message gives the same cleaner diagnostic.
        let engine = Arc::try_unwrap(self.engine)
            .unwrap_or_else(|_| {
                panic!("internal invariant: engine Arc was captured by shared reference")
            })
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
            generator_timeout_ms,
            ..self
        }
    }

    /// Update runtime-configurable fields without restarting the proxy.
    /// Called by the config file watcher when config.toml changes on disk.
    /// Returns cleanup bytes to write to stdout (e.g. popup clear on auto_trigger toggle).
    pub fn update_config(
        &mut self,
        theme: PopupTheme,
        keybindings: Keybindings,
        trigger_chars: &[char],
        max_visible: usize,
        feedback_dismiss_ms: u16,
        auto_trigger: bool,
    ) -> Vec<u8> {
        let mut cleanup = Vec::new();

        // If auto_trigger is being disabled, tear down all pending state —
        // not just the visible popup.  A pending trigger_requested or in-flight
        // dynamic_task can survive without the popup being visible (e.g. the
        // debounce timer set trigger_requested but trigger() hasn't fired yet).
        if self.auto_trigger && !auto_trigger {
            if self.visible {
                if let Some(ref layout) = self.last_layout {
                    clear_popup(&mut cleanup, layout, &self.terminal_profile);
                }
                self.visible = false;
                self.suggestions.clear();
                self.overlay.reset();
                self.last_layout = None;
            }
            if let Some(handle) = self.dynamic_task.take() {
                handle.abort();
            }
            self.dynamic_rx = None;
            self.dynamic_ctx = None;
            self.feedback = AsyncFeedback::Idle;
            self.pending_failed.clear();
            self.pending_empty_count = 0;
            self.trigger_requested = false;
        }

        self.theme = theme;
        self.keybindings = keybindings;
        self.trigger_chars = trigger_chars.to_vec();
        self.max_visible = max_visible;
        self.feedback_dismiss_ms = feedback_dismiss_ms;
        self.auto_trigger = auto_trigger;

        cleanup
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

    pub fn auto_trigger_enabled(&self) -> bool {
        self.auto_trigger
    }

    // The #[doc(hidden)] pub accessors below exist solely so integration tests
    // can simulate generator drift / drive the rx channel directly. They are
    // not part of the supported API.

    /// Restore a channel receiver that was taken out for an awaited bounded-block
    /// window but was not consumed (e.g. due to keystroke cancellation). This
    /// allows `dynamic_merge_loop` to pick up the result when the generator
    /// eventually completes.
    #[doc(hidden)]
    pub fn restore_dynamic_rx(&mut self, rx: mpsc::Receiver<DynamicResult>) {
        self.dynamic_rx = Some(rx);
    }

    /// Returns whether the `dynamic_rx` channel is set (a generator is pending).
    #[doc(hidden)]
    pub fn has_dynamic_rx(&self) -> bool {
        self.dynamic_rx.is_some()
    }

    /// Returns whether `dynamic_task` is set (a generator task handle is owned).
    #[doc(hidden)]
    pub fn has_dynamic_task(&self) -> bool {
        self.dynamic_task.is_some()
    }

    /// Takes the `dynamic_rx` channel out of the handler, leaving `None`.
    #[doc(hidden)]
    pub fn take_dynamic_rx(&mut self) -> Option<mpsc::Receiver<DynamicResult>> {
        self.dynamic_rx.take()
    }

    #[doc(hidden)]
    pub fn feedback_kind(&self) -> &AsyncFeedback {
        &self.feedback
    }

    /// Returns the current suggestions slice (read-only).
    #[doc(hidden)]
    pub fn current_suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    /// Set the `spawned_generation` field, simulating a `spawn_generators` run
    /// for the current `buffer_generation`.
    #[doc(hidden)]
    pub fn set_spawned_generation(&mut self, gen: u64) {
        self.spawned_generation = gen;
    }

    #[doc(hidden)]
    pub fn set_buffer_generation(&mut self, gen: u64) {
        self.buffer_generation = gen;
    }

    #[doc(hidden)]
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Prime `dynamic_ctx` to the "no context" state that matches an empty
    /// buffer, bypassing `spawn_generators`.
    #[doc(hidden)]
    pub fn prime_dynamic_ctx_for_empty_buffer(&mut self) {
        let base_ctx = gc_buffer::parse_command_context("", 0);
        self.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));
    }

    /// Prime `dynamic_ctx` from an arbitrary buffer + cursor so tests can
    /// drive the staleness check against a non-empty live buffer.
    #[doc(hidden)]
    pub fn prime_dynamic_ctx_for_buffer(&mut self, buffer: &str, cursor: usize) {
        let base_ctx = gc_buffer::parse_command_context(buffer, cursor);
        self.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));
    }

    /// Seed `dynamic_task` with a no-op spawn handle so tests can verify
    /// abort/clear semantics without spinning up a real generator.
    #[doc(hidden)]
    pub fn seed_dynamic_task_noop(&mut self) {
        self.dynamic_task = Some(tokio::spawn(async {}));
    }

    /// Read the last trigger fingerprint so tests can assert `apply_block_result`
    /// stamped it on success / left it untouched on stale.
    #[doc(hidden)]
    pub fn last_trigger_fingerprint(&self) -> Option<(u64, usize)> {
        self.last_trigger_fingerprint
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
        let visible_rows = self.effective_navigation_visible_rows(parser);

        // Configurable actions checked first via if-chain
        if key == &self.keybindings.navigate_up {
            self.overlay.move_up();
            self.render(parser, stdout);
            return Vec::new();
        }
        if key == &self.keybindings.navigate_down {
            self.overlay.move_down(self.suggestions.len(), visible_rows);
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

        match key {
            KeyEvent::PageUp => {
                self.overlay.move_page_up(visible_rows);
                self.render(parser, stdout);
                return Vec::new();
            }
            KeyEvent::PageDown => {
                self.overlay
                    .move_page_down(self.suggestions.len(), visible_rows);
                self.render(parser, stdout);
                return Vec::new();
            }
            KeyEvent::Home
            | KeyEvent::HomeCsiTilde
            | KeyEvent::HomeCsi7Tilde
            | KeyEvent::HomeSs3 => {
                self.overlay.move_home(self.suggestions.len());
                self.render(parser, stdout);
                return Vec::new();
            }
            KeyEvent::End | KeyEvent::EndCsiTilde | KeyEvent::EndCsi8Tilde | KeyEvent::EndSs3 => {
                self.overlay.move_end(self.suggestions.len(), visible_rows);
                self.render(parser, stdout);
                return Vec::new();
            }
            _ => {}
        }

        // Remaining structural keys/default visible-popup handling.
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

    fn effective_navigation_visible_rows(&self, parser: &Arc<Mutex<TerminalParser>>) -> usize {
        let border_pad: u16 = if self.theme.borders { 2 } else { 0 };
        let min_screen = 1 + border_pad;

        let screen_rows = match parser.lock() {
            Ok(p) => p.state().screen_dimensions().0,
            Err(e) => {
                tracing::warn!(
                    "parser mutex poisoned while computing popup navigation height: {e} — \
                     using configured max_visible"
                );
                return self.max_visible.max(1);
            }
        };

        if screen_rows > min_screen {
            self.max_visible
                .min((screen_rows - 1 - border_pad) as usize)
                .max(1)
        } else {
            self.max_visible.max(1)
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

        // Single parser lock for both the accept computation AND the
        // CD-chaining prediction. Prevents TOCTOU between the two reads and
        // mirrors the lock-ordering discipline established in proxy.rs.
        //
        // Poison handling mirrors render/accept_suggestion: if the parser
        // mutex is poisoned we can't read the buffer, so dismiss the popup
        // and return empty bytes. Failing here must not crash the proxy.
        let mut p = match parser.lock() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "parser mutex poisoned in accept_with_chaining: {e} — \
                     dropping accept"
                );
                self.dismiss(stdout);
                return Vec::new();
            }
        };

        let (forward, cwd, cursor_position, screen_dimensions) =
            match self.accept_suggestion_locked(&p) {
                Some(tuple) => tuple,
                None => {
                    drop(p);
                    self.dismiss(stdout);
                    return Vec::new();
                }
            };

        // History entries never chain — they're full commands, not directory paths.
        if selected_kind == gc_suggest::SuggestionKind::History {
            drop(p);
            self.dismiss(stdout);
            return forward;
        }

        if !is_dir {
            drop(p);
            self.dismiss(stdout);
            // Append trailing space so the user can immediately type the next
            // argument. Skip for text ending in '=' (flag expecting value like
            // --output=) since the user needs to type the value directly.
            if !selected_text.ends_with('=') {
                return [forward, vec![b' ']].concat();
            }
            return forward;
        }

        // CD chaining: predict the buffer after acceptance and immediately
        // show next-level suggestions. Avoids timing issues with the shell's
        // OSC 7770 roundtrip. Reuses the already-held parser lock for the
        // prediction read and the `predict_command_buffer` mutation.
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

        let predicted_ctx = parse_command_context(&predicted, new_cursor);
        let predicted_buffer = predicted.clone();

        // Update parser with predicted buffer so subsequent accept computes
        // correct current_word.
        p.state_mut().predict_command_buffer(predicted, new_cursor);
        drop(p);

        let (cr, cc) = cursor_position;
        let (sr, sc) = screen_dimensions;

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
                let mut buf = [0u8; 4];
                let forward = c.encode_utf8(&mut buf).as_bytes().to_vec();
                if self.auto_trigger && self.trigger_chars.contains(c) {
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
        // Poison handling mirrors render/accept_suggestion: trigger() is the
        // main entry point of the suggestion pipeline (debounce loop, Task B
        // buffer_dirty/cwd_dirty, SIGWINCH). If the parser mutex is poisoned
        // we can't read the buffer or cursor, so log and bail out — the next
        // PTY input drives a retry. Propagating the poison here would crash
        // the proxy.
        let (buffer, cursor, cwd, cursor_row, cursor_col, screen_rows, screen_cols) =
            match parser.lock() {
                Ok(mut p) => {
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
                }
                Err(e) => {
                    tracing::warn!("parser mutex poisoned in trigger: {e} — skipping trigger");
                    return;
                }
            };

        if buffer.is_empty() {
            if self.visible {
                self.dismiss(stdout);
            }
            return;
        }

        // Idempotency guard: if the buffer + cursor are unchanged since the
        // last trigger that populated a still-visible popup, skip the whole
        // trigger body. Two reasons:
        //   1. `suggest_sync` would return the same results — redundant work.
        //   2. The empty-sync + no-async branch below calls `dismiss()`,
        //      which would nuke a popup that had been populated by a prior
        //      trigger's async generator merge (the sync pass sees empty,
        //      but the visible content came from async). See the bug-repro
        //      test `test_trigger_idempotent_when_buffer_unchanged`.
        // `dismiss()` invalidates the fingerprint, and a genuine buffer
        // edit produces a different fingerprint — so ESC-dismiss and real
        // edits both take the full trigger path as before. The async
        // merge path (`try_merge_dynamic`) is separate and unaffected.
        let fingerprint = buffer_fingerprint(&buffer, cursor);
        if self.visible && self.last_trigger_fingerprint == Some(fingerprint) {
            return;
        }

        let ctx = parse_command_context(&buffer, cursor);

        // Abort any in-flight generator task before dropping the receiver,
        // otherwise the spawned task leaks (tx.send blocks on dropped rx).
        if let Some(handle) = self.dynamic_task.take() {
            handle.abort();
        }
        self.dynamic_rx = None;
        self.dynamic_ctx = None;
        self.feedback = AsyncFeedback::Idle;
        self.pending_failed.clear();
        self.pending_empty_count = 0;

        self.buffer_generation = self.buffer_generation.wrapping_add(1);

        let sync_result = self.engine.suggest_sync(&ctx, &cwd, &buffer);

        match sync_result {
            Ok(result) if !result.suggestions.is_empty() => {
                self.suggestions = result.suggestions;
                self.overlay.reset();
                self.visible = true;
                self.spawn_generators(
                    result.script_generators,
                    result.git_generators,
                    result.provider_generators,
                    &ctx,
                    &cwd,
                );
                self.render_at(stdout, cursor_row, cursor_col, screen_rows, screen_cols);
                self.last_trigger_fingerprint = Some(fingerprint);
            }
            Ok(result) => {
                let has_async = !result.script_generators.is_empty()
                    || !result.git_generators.is_empty()
                    || !result.provider_generators.is_empty();
                if has_async {
                    // No static suggestions but generators are pending.
                    // If a popup is currently visible (e.g. from a previous
                    // trigger with static results), dismiss it first — the
                    // old popup's screen contents, selection state, and any
                    // in-flight task must all be cleared before spawning new
                    // generators. dismiss() handles clear_popup, abort of
                    // dynamic_task, and resetting visible/suggestions/layout.
                    if self.visible {
                        self.dismiss(stdout);
                    }
                    // Don't set visible yet — that would intercept navigation
                    // keys while the popup is empty. The popup becomes visible
                    // when try_merge_dynamic receives non-empty results.
                    self.spawn_generators(
                        result.script_generators,
                        result.git_generators,
                        result.provider_generators,
                        &ctx,
                        &cwd,
                    );
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

    /// Synchronous phase of the bounded-block trigger for the debounce path.
    ///
    /// Runs `suggest_sync`, paints sync-only results, and spawns generators.
    /// If `render_block_ms > 0` and a high-priority generator is pending,
    /// returns `TriggerPrepared::NeedsBlock` with the channel receiver so the
    /// debounce loop can `await` the bounded window **outside** the
    /// `std::sync::Mutex` lock. Otherwise returns `TriggerPrepared::Painted`.
    ///
    /// Render bytes are appended to `stdout`. Caller writes them to stdout
    /// after releasing the lock.
    pub fn prepare_trigger_with_block(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut Vec<u8>,
    ) -> TriggerPrepared {
        let block_ms = self.render_block_ms;

        // Extract parser state.
        let (buffer, cursor, cwd, cursor_row, cursor_col, screen_rows, screen_cols) =
            match parser.lock() {
                Ok(mut p) => {
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
                }
                Err(e) => {
                    tracing::warn!(
                        "parser mutex poisoned in prepare_trigger_with_block: {e} — skipping"
                    );
                    return TriggerPrepared::Painted;
                }
            };

        if buffer.is_empty() {
            if self.visible {
                self.dismiss(stdout);
            }
            return TriggerPrepared::Painted;
        }

        let fingerprint = buffer_fingerprint(&buffer, cursor);
        if self.visible && self.last_trigger_fingerprint == Some(fingerprint) {
            return TriggerPrepared::Painted;
        }

        let ctx = parse_command_context(&buffer, cursor);

        if let Some(handle) = self.dynamic_task.take() {
            handle.abort();
        }
        self.dynamic_rx = None;
        self.dynamic_ctx = None;
        self.feedback = AsyncFeedback::Idle;
        self.pending_failed.clear();
        self.pending_empty_count = 0;
        self.buffer_generation = self.buffer_generation.wrapping_add(1);

        let result = match self.engine.suggest_sync(&ctx, &cwd, &buffer) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("suggest_sync failed in prepare_trigger_with_block: {e}");
                if self.visible {
                    self.dismiss(stdout);
                }
                return TriggerPrepared::Painted;
            }
        };

        let has_async = !result.script_generators.is_empty()
            || !result.git_generators.is_empty()
            || !result.provider_generators.is_empty();
        let needs_block = block_ms > 0 && has_async && result.has_pending_high_priority();

        let sync_suggestions = result.suggestions;

        // Dismiss any stale popup BEFORE spawning generators. `dismiss()`
        // aborts `dynamic_task` and clears `dynamic_rx`, which would close
        // the channel a freshly-spawned generator writes to and leave Phase
        // 2 awaiting a dead receiver.
        if sync_suggestions.is_empty() && self.visible {
            self.dismiss(stdout);
        }

        // Spawn generators (consumes script/git/provider vecs).
        if has_async {
            self.spawn_generators(
                result.script_generators,
                result.git_generators,
                result.provider_generators,
                &ctx,
                &cwd,
            );
        }

        if needs_block {
            // Take the rx out of self. The caller awaits it outside the lock,
            // then calls apply_block_result to merge and repaint.
            if let Some(rx) = self.dynamic_rx.take() {
                // Paint sync-only to give immediate feedback while waiting.
                if !sync_suggestions.is_empty() {
                    self.suggestions = sync_suggestions.clone();
                    self.overlay.reset();
                    self.visible = true;
                    self.render_at(stdout, cursor_row, cursor_col, screen_rows, screen_cols);
                }

                return TriggerPrepared::NeedsBlock {
                    rx,
                    sync_suggestions,
                    block_ms,
                    cursor_row,
                    cursor_col,
                    screen_rows,
                    screen_cols,
                    fingerprint,
                    current_word: ctx.current_word.clone(),
                };
            }
        }

        // No block needed — paint sync-only and let dynamic_merge_loop handle async.
        if !sync_suggestions.is_empty() {
            self.suggestions = sync_suggestions;
            self.overlay.reset();
            self.visible = true;
            self.render_at(stdout, cursor_row, cursor_col, screen_rows, screen_cols);
            self.last_trigger_fingerprint = Some(fingerprint);
        }

        TriggerPrepared::Painted
    }

    /// Apply the result of the bounded-block window after the debounce loop
    /// awaited the async generator outside the mutex lock.
    #[allow(clippy::too_many_arguments)] // all args are genuinely independent
    pub fn apply_block_result(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut Vec<u8>,
        maybe_async: Option<DynamicResult>,
        rx_after_recv: Option<mpsc::Receiver<DynamicResult>>,
        rx_on_timeout: Option<mpsc::Receiver<DynamicResult>>,
        sync_suggestions: Vec<Suggestion>,
        cursor_row: u16,
        cursor_col: u16,
        screen_rows: u16,
        screen_cols: u16,
        fingerprint: (u64, usize),
        current_word: &str,
    ) {
        let was_timeout = rx_on_timeout.is_some();
        if let Some(rx) = rx_on_timeout {
            self.dynamic_rx = Some(rx);
        }

        if was_timeout {
            // Sync-only was already painted in prepare_trigger_with_block;
            // dynamic_merge_loop will merge the late result.
            self.last_trigger_fingerprint = Some(fingerprint);
            return;
        }

        let mut messages = maybe_async.into_iter().collect::<Vec<_>>();
        let mut disconnected = rx_after_recv.is_none();
        if let Some(mut rx) = rx_after_recv {
            loop {
                match rx.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(mpsc::error::TryRecvError::Empty) => {
                        self.dynamic_rx = Some(rx);
                        break;
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.dynamic_rx = None;
            self.dynamic_task = None;
        }
        let aggregation = AsyncFeedback::aggregate(messages);
        self.pending_failed.extend(aggregation.failed);
        self.pending_empty_count += aggregation.empty_count;
        if aggregation.loaded.is_empty() {
            if disconnected {
                self.dynamic_ctx = None;
            }
            let now = std::time::Instant::now();
            self.feedback = if !disconnected {
                self.feedback.clone()
            } else if !self.pending_failed.is_empty() && !sync_suggestions.is_empty() {
                AsyncFeedback::PartialError {
                    failed: std::mem::take(&mut self.pending_failed),
                    since: now,
                }
            } else if self.pending_empty_count > 0 && !sync_suggestions.is_empty() {
                AsyncFeedback::Idle
            } else {
                AsyncFeedback::terminal_for_outcome(
                    false,
                    &self.pending_failed,
                    self.pending_empty_count,
                    now,
                )
            };
            if disconnected {
                self.pending_failed.clear();
                self.pending_empty_count = 0;
            }
            self.feedback_tick_notify.notify_one();
            if self.visible || self.feedback.is_terminal() || self.feedback.is_loading() {
                self.render(parser, stdout);
            }
            self.last_trigger_fingerprint = Some(fingerprint);
            return;
        }
        let async_results = aggregation.loaded;
        let now = std::time::Instant::now();
        self.feedback = if disconnected {
            AsyncFeedback::terminal_for_outcome(
                !async_results.is_empty(),
                &self.pending_failed,
                self.pending_empty_count,
                now,
            )
        } else {
            self.feedback.clone()
        };
        if disconnected {
            self.pending_failed.clear();
            self.pending_empty_count = 0;
        }
        self.feedback_tick_notify.notify_one();

        // Re-check that the buffer hasn't drifted while we were awaiting
        // the generator. The captured `cursor_row` / `cursor_col` /
        // `current_word` / `fingerprint` are all from the spawn site
        // ~block_ms ago; merging against a freshly-typed buffer would rank
        // the wrong query and paint at the wrong cursor row.
        let live_word = match self.check_merge_staleness(parser) {
            MergeFreshness::Fresh { current_word } => current_word,
            MergeFreshness::Stale => {
                self.dynamic_rx = None;
                self.dynamic_ctx = None;
                self.dynamic_task = None;
                self.feedback = AsyncFeedback::Idle;
                self.pending_failed.clear();
                self.pending_empty_count = 0;
                if self.visible {
                    self.render(parser, stdout);
                }
                return;
            }
            MergeFreshness::PoisonedLock => return,
        };

        let mut all = sync_suggestions;
        let extras = merge_dedup_against(&all, async_results);
        all.extend(extras);
        // Mirror try_merge_dynamic: rank against the LIVE query so a
        // user keystroke during the bounded wait re-filters the pool
        // instead of ranking against the stale spawn-time word. When the
        // user has typed nothing we keep the full pool sorted by priority
        // (truncating with an empty query would alphabetically evict
        // high-value candidates from large dynamic pools — see the long
        // comment in try_merge_dynamic). When they have typed, fuzzy rank
        // against the query and cap at max_visible * 5.
        all = if live_word.is_empty() {
            all.sort_by(|a, b| {
                gc_suggest::priority::effective(b)
                    .cmp(&gc_suggest::priority::effective(a))
                    .then_with(|| a.text.cmp(&b.text))
            });
            all
        } else {
            gc_suggest::fuzzy::rank(&live_word, all, self.max_visible * 5)
        };

        self.suggestions = all;
        self.overlay.reset();
        self.visible = true;
        if disconnected {
            self.dynamic_ctx = None;
        }
        // Render against the live cursor/screen geometry: when the buffer
        // drifted (live_word differs from spawn-time current_word) the
        // captured cursor row is no longer where the prompt sits.
        if live_word == current_word {
            self.render_at(stdout, cursor_row, cursor_col, screen_rows, screen_cols);
        } else {
            self.render(parser, stdout);
        }
        self.last_trigger_fingerprint = Some(fingerprint);
    }

    /// Spawn an async task to run pre-resolved generators (script + git).
    /// Results arrive via `dynamic_rx` and Task E renders them via `dynamic_notify`.
    ///
    /// Takes `Arc<GeneratorSpec>` rather than the bare struct so the
    /// `SyncResult` → `spawn_generators` → `run_generators` chain is a
    /// refcount bump instead of a deep clone of `Vec<Transform>` + argv on
    /// every keystroke trigger.
    fn spawn_generators(
        &mut self,
        script_generators: Vec<std::sync::Arc<gc_suggest::specs::GeneratorSpec>>,
        git_generators: Vec<gc_suggest::git::GitQueryKind>,
        provider_generators: Vec<gc_suggest::providers::ProviderKind>,
        ctx: &gc_buffer::CommandContext,
        cwd: &std::path::Path,
    ) {
        if script_generators.is_empty()
            && git_generators.is_empty()
            && provider_generators.is_empty()
        {
            return;
        }
        // Snapshot the command context so try_merge_dynamic can drop results
        // if the user typed a different command/subcommand/flag while
        // generators were running. A script_template only depends on
        // current_word if its template actually contains `{current_token}`
        // — templates that only use `{prev_token}` or no placeholders at
        // all don't need current_word to be pinned.
        let uses_current_word = script_generators.iter().any(|gen| {
            gen.script_template
                .as_ref()
                .is_some_and(|tpl| tpl.iter().any(|part| part.contains("{current_token}")))
        });
        self.dynamic_ctx = Some(DynamicCtxSnapshot::capture(ctx, uses_current_word));
        self.spawned_generation = self.buffer_generation;
        let (tx, rx) = mpsc::channel::<DynamicResult>(8);
        self.dynamic_rx = Some(rx);
        self.feedback = AsyncFeedback::Loading {
            spawned_at: std::time::Instant::now(),
        };
        self.pending_failed.clear();
        self.pending_empty_count = 0;
        self.feedback_tick_notify.notify_one();
        let engine = Arc::clone(&self.engine);
        let ctx = ctx.clone();
        let cwd = cwd.to_path_buf();
        let timeout = self.generator_timeout_ms;
        let notify = Arc::clone(&self.dynamic_notify);
        let handle = tokio::spawn(async move {
            // Build ProviderCtx once — the env snapshot is shared across
            // every provider in this resolution pass. Skip the
            // `std::env::vars().collect()` walk when no provider is
            // scheduled this pass: script-only specs hit this branch on
            // every keystroke, and no current provider reads `ctx.env`,
            // so the collected map would be dead weight on the hot path.
            let env = Arc::new(build_env_snapshot(!provider_generators.is_empty()));
            let provider_ctx = Arc::new(gc_suggest::providers::ProviderCtx {
                cwd: cwd.clone(),
                env,
                current_token: ctx.current_word.clone(),
            });

            // Per-kind dispatch so each git/provider failure surfaces with its own ProviderTag.
            let git_engine = Arc::clone(&engine);
            let git_cwd = cwd.clone();
            let git_query = ctx.current_word.clone();
            let git_kinds = git_generators.clone();
            let git_fut = async move {
                let mut out = Vec::with_capacity(git_kinds.len());
                for kind in git_kinds {
                    let res = git_engine
                        .resolve_git_kind(kind, &git_cwd, &git_query)
                        .await;
                    out.push((kind, res));
                }
                out
            };
            let provider_engine = Arc::clone(&engine);
            let provider_query = ctx.current_word.clone();
            let provider_kinds = provider_generators.clone();
            let provider_ctx_for_fut = Arc::clone(&provider_ctx);
            let provider_fut = async move {
                let mut out = Vec::with_capacity(provider_kinds.len());
                for kind in provider_kinds {
                    let res = provider_engine
                        .resolve_provider_kind(kind, &provider_ctx_for_fut, &provider_query)
                        .await;
                    out.push((kind, res));
                }
                out
            };
            let (script_res, git_results, provider_results) = tokio::join!(
                engine.run_generators(&script_generators, &ctx, &cwd, timeout),
                git_fut,
                provider_fut,
            );

            if !script_generators.is_empty() {
                let provider = ProviderTag::Script(ctx.command.clone().unwrap_or_default());
                let message = match script_res {
                    Ok(results) if results.is_empty() => DynamicResult::Empty { provider },
                    Ok(results) => DynamicResult::Loaded {
                        provider,
                        suggestions: results,
                    },
                    Err(e) => {
                        tracing::warn!("dynamic suggestions failed: {e}");
                        DynamicResult::Error {
                            provider,
                            message: e.to_string(),
                        }
                    }
                };
                if tx.send(message).await.is_err() {
                    return;
                }
            }

            for (kind, result) in git_results {
                let provider = ProviderTag::Git(kind);
                let message = match result {
                    Ok(results) if results.is_empty() => DynamicResult::Empty { provider },
                    Ok(results) => DynamicResult::Loaded {
                        provider,
                        suggestions: results,
                    },
                    Err(e) => {
                        tracing::warn!("git suggestions failed ({kind:?}): {e}");
                        DynamicResult::Error {
                            provider,
                            message: e.to_string(),
                        }
                    }
                };
                if tx.send(message).await.is_err() {
                    return;
                }
            }

            for (kind, result) in provider_results {
                let provider = ProviderTag::Provider(kind);
                let message = match result {
                    Ok(results) if results.is_empty() => DynamicResult::Empty { provider },
                    Ok(results) => DynamicResult::Loaded {
                        provider,
                        suggestions: results,
                    },
                    Err(e) => {
                        tracing::warn!("provider suggestions failed ({kind:?}): {e}");
                        DynamicResult::Error {
                            provider,
                            message: e.to_string(),
                        }
                    }
                };
                if tx.send(message).await.is_err() {
                    return;
                }
            }
            // Drop tx BEFORE notifying so Task E sees Disconnected on
            // the first try_recv after wake. Otherwise on a multi-threaded
            // runtime Task E can wake and read rx while this task is still
            // executing its local drops, getting Empty instead of
            // Disconnected — and Empty leaves dynamic_rx = Some, which
            // pins the loading indicator on forever with no further
            // notifications coming.
            drop(tx);
            // Always notify so Task E clears the loading indicator,
            // even when generators returned empty or errored.
            notify.notify_one();
        });
        self.dynamic_task = Some(handle);
    }

    /// Re-acquire parser state and verify that an in-flight generator's
    /// spawn-time context still matches the user's current buffer.
    ///
    /// On `Fresh`, the caller proceeds with the merge and uses the returned
    /// live `current_word`. On `Stale`, the caller drops the results and
    /// repaints to clear the loading indicator. On `PoisonedLock`, the caller
    /// returns without re-rendering — the next `parser.lock()` would just
    /// log-and-skip again.
    fn check_merge_staleness(&mut self, parser: &Arc<Mutex<TerminalParser>>) -> MergeFreshness {
        let current_ctx = {
            let p = match parser.lock() {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        "parser lock poisoned during dynamic merge re-rank: {e} — \
                         disabling dynamic_rx"
                    );
                    self.dynamic_rx = None;
                    self.dynamic_ctx = None;
                    self.dynamic_task = None;
                    return MergeFreshness::PoisonedLock;
                }
            };
            let state = p.state();
            let buffer = state.command_buffer().unwrap_or("");
            let cursor = state.buffer_cursor();
            parse_command_context(buffer, cursor)
        };
        let current_word = current_ctx.current_word.clone();

        if self.spawned_generation != self.buffer_generation {
            self.dynamic_ctx = None;
            return MergeFreshness::Stale;
        }

        let stale = match &self.dynamic_ctx {
            Some(spawned) => spawned.is_stale_against(&current_ctx),
            None => true,
        };
        if stale {
            return MergeFreshness::Stale;
        }
        MergeFreshness::Fresh { current_word }
    }

    /// Check for pending dynamic (script generator) results and merge them
    /// into the current suggestions. Returns `true` if the popup was updated.
    pub fn try_merge_dynamic(
        &mut self,
        parser: &Arc<Mutex<TerminalParser>>,
        stdout: &mut dyn Write,
    ) -> bool {
        let mut messages = Vec::new();
        let mut disconnected = false;
        {
            let rx = match self.dynamic_rx.as_mut() {
                Some(rx) => rx,
                None => return false,
            };
            loop {
                match rx.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        if messages.is_empty() {
            if disconnected {
                self.dynamic_rx = None;
                self.dynamic_ctx = None;
                self.dynamic_task = None;
                let now = std::time::Instant::now();
                self.feedback = if !self.pending_failed.is_empty() && !self.suggestions.is_empty() {
                    AsyncFeedback::PartialError {
                        failed: std::mem::take(&mut self.pending_failed),
                        since: now,
                    }
                } else if self.pending_empty_count > 0 && !self.suggestions.is_empty() {
                    AsyncFeedback::Idle
                } else {
                    AsyncFeedback::terminal_for_outcome(
                        false,
                        &self.pending_failed,
                        self.pending_empty_count,
                        now,
                    )
                };
                self.pending_failed.clear();
                self.pending_empty_count = 0;
                self.feedback_tick_notify.notify_one();
                if self.visible {
                    self.render(parser, stdout);
                }
            }
            return false;
        }

        let aggregation = AsyncFeedback::aggregate(messages);
        self.pending_failed.extend(aggregation.failed);
        self.pending_empty_count += aggregation.empty_count;

        if disconnected {
            self.dynamic_rx = None;
            self.dynamic_task = None;
        }

        if aggregation.loaded.is_empty() {
            if disconnected {
                self.dynamic_ctx = None;
            }
            let now = std::time::Instant::now();
            self.feedback = if !disconnected {
                self.feedback.clone()
            } else if !self.pending_failed.is_empty() && !self.suggestions.is_empty() {
                AsyncFeedback::PartialError {
                    failed: std::mem::take(&mut self.pending_failed),
                    since: now,
                }
            } else if self.pending_empty_count > 0 && !self.suggestions.is_empty() {
                AsyncFeedback::Idle
            } else {
                AsyncFeedback::terminal_for_outcome(
                    false,
                    &self.pending_failed,
                    self.pending_empty_count,
                    now,
                )
            };
            if disconnected {
                self.pending_failed.clear();
                self.pending_empty_count = 0;
            }
            self.feedback_tick_notify.notify_one();
            self.render(parser, stdout);
            return !matches!(self.feedback, AsyncFeedback::Idle);
        }

        {
            let dynamic_results = aggregation.loaded;
            let now = std::time::Instant::now();
            let previous_feedback = self.feedback.clone();
            self.feedback = if disconnected {
                AsyncFeedback::terminal_for_outcome(
                    !dynamic_results.is_empty(),
                    &self.pending_failed,
                    self.pending_empty_count,
                    now,
                )
            } else {
                previous_feedback
            };
            if disconnected {
                self.pending_failed.clear();
                self.pending_empty_count = 0;
            }
            self.feedback_tick_notify.notify_one();
            if disconnected {
                self.dynamic_rx = None;
                // The generator task has completed (it sent its results and
                // dropped tx). The JoinHandle is now a no-op for `.abort()`
                // but we still clear it so dismiss()/trigger() don't rely
                // on an already-completed handle for their orphan-task
                // cleanup guarantees.
                self.dynamic_task = None;
            }
            let current_word = match self.check_merge_staleness(parser) {
                MergeFreshness::Fresh { current_word } => current_word,
                MergeFreshness::Stale => {
                    self.dynamic_rx = None;
                    self.dynamic_ctx = None;
                    self.dynamic_task = None;
                    self.feedback = AsyncFeedback::Idle;
                    self.pending_failed.clear();
                    self.pending_empty_count = 0;
                    if self.visible {
                        self.render(parser, stdout);
                    }
                    return false;
                }
                MergeFreshness::PoisonedLock => return false,
            };

            // Activate popup if it wasn't visible yet (async-only path:
            // no static suggestions, generators produced the results).
            if !self.visible {
                self.visible = true;
                self.overlay.reset();
            }

            let extras = merge_dedup_against(&self.suggestions, dynamic_results);
            self.suggestions.extend(extras);
            let merged = std::mem::take(&mut self.suggestions);
            // Empty query: keep the full pool untruncated so a non-empty re-rank can still recover late candidates.
            self.suggestions = if current_word.is_empty() {
                // Re-sort so higher-priority dynamic arrivals outrank residual sync items.
                let mut m = merged;
                m.sort_by(|a, b| {
                    gc_suggest::priority::effective(b)
                        .cmp(&gc_suggest::priority::effective(a))
                        .then_with(|| a.text.cmp(&b.text))
                });
                m
            } else {
                gc_suggest::fuzzy::rank(&current_word, merged, self.max_visible * 5)
            };

            if self.suggestions.is_empty() {
                self.dismiss(stdout);
                return true;
            }
            if disconnected {
                self.dynamic_ctx = None;
            }

            self.render(parser, stdout);
            true
        }
    }

    fn render(&mut self, parser: &Arc<Mutex<TerminalParser>>, stdout: &mut dyn Write) {
        // Poison handling mirrors Task B in proxy.rs: if the parser mutex
        // is poisoned (another task panicked while holding it), log and
        // skip this render rather than propagating the panic. The popup
        // will simply not update on this tick; the next render attempt is
        // driven by further PTY input.
        let (cursor_row, cursor_col, screen_rows, screen_cols) = match parser.lock() {
            Ok(p) => {
                let state = p.state();
                let (cr, cc) = state.cursor_position();
                let (sr, sc) = state.screen_dimensions();
                (cr, cc, sr, sc)
            }
            Err(e) => {
                tracing::warn!("parser mutex poisoned in render: {e} — skipping render");
                return;
            }
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

        let feedback = self.current_feedback_kind();
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
            feedback,
            &self.terminal_profile,
        );
        let _ = stdout.write_all(&buf);
        let _ = stdout.flush();
        self.scroll_deficit = layout.scroll_deficit;
        self.last_layout = Some(layout);
    }

    fn current_feedback_kind(&self) -> FeedbackKind {
        match &self.feedback {
            AsyncFeedback::Idle => FeedbackKind::None,
            AsyncFeedback::Loading { spawned_at } => {
                let elapsed_ms = spawned_at.elapsed().as_millis() as u64;
                FeedbackKind::Loading {
                    frame: ((elapsed_ms / 80) % 10) as u8,
                }
            }
            AsyncFeedback::Empty { .. } => FeedbackKind::Empty,
            AsyncFeedback::Error { failed, .. } => {
                if failed.len() > 1 {
                    FeedbackKind::PartialError {
                        providers: failed.len().min(u8::MAX as usize) as u8,
                    }
                } else {
                    FeedbackKind::Error {
                        provider: if self.theme.show_provider_errors {
                            failed.first().cloned().unwrap_or_default()
                        } else {
                            String::new()
                        },
                    }
                }
            }
            AsyncFeedback::PartialError { failed, .. } => FeedbackKind::PartialError {
                providers: failed.len().min(u8::MAX as usize) as u8,
            },
        }
    }

    pub fn render_indicator_only(&mut self, stdout: &mut dyn Write) {
        let Some(layout) = self.last_layout.clone() else {
            return;
        };
        let feedback = self.current_feedback_kind();
        let mut buf = Vec::new();
        render_indicator_row(&mut buf, &layout, &self.theme, feedback);
        let _ = stdout.write_all(&buf);
        let _ = stdout.flush();
    }

    pub fn clear_expired_feedback(&mut self, stdout: &mut dyn Write) -> bool {
        if self.feedback_dismiss_ms == 0 {
            return false;
        }
        if !self.feedback.is_terminal() {
            return false;
        }
        let Some(since) = self.feedback.since() else {
            return false;
        };
        if since.elapsed() < std::time::Duration::from_millis(self.feedback_dismiss_ms as u64) {
            return false;
        }
        // PartialError + suggestions present: drop only the indicator row, not the popup.
        if matches!(self.feedback, AsyncFeedback::PartialError { .. })
            && !self.suggestions.is_empty()
        {
            self.feedback = AsyncFeedback::Idle;
            if let Some(mut layout) = self.last_layout.clone() {
                if layout.height > 0 {
                    let borders = self.theme.borders;
                    let indicator_row = layout.start_row + layout.height - 1 - u16::from(borders);
                    let mut buf = Vec::with_capacity(layout.width as usize * 4 + 64);
                    buf.extend_from_slice(b"\x1b[s"); // save cursor
                    let _ = write!(
                        &mut buf,
                        "\x1b[{};{}H",
                        indicator_row + 1,
                        layout.start_col + 1
                    );
                    buf.extend(std::iter::repeat_n(b' ', layout.width as usize));
                    if borders {
                        let displaced_border_row = layout.start_row + layout.height - 1;
                        let _ = write!(
                            &mut buf,
                            "\x1b[{};{}H",
                            displaced_border_row + 1,
                            layout.start_col + 1
                        );
                        buf.extend(std::iter::repeat_n(b' ', layout.width as usize));
                        let _ = write!(
                            &mut buf,
                            "\x1b[{};{}H",
                            indicator_row + 1,
                            layout.start_col + 1
                        );
                        if !self.theme.border_on.is_empty() {
                            buf.extend_from_slice(&self.theme.border_on);
                        }
                        buf.extend_from_slice("╰".as_bytes());
                        let content_width = layout.width.saturating_sub(2);
                        for _ in 0..content_width {
                            buf.extend_from_slice("─".as_bytes());
                        }
                        buf.extend_from_slice("╯".as_bytes());
                        buf.extend_from_slice(b"\x1b[0m");
                    }
                    buf.extend_from_slice(b"\x1b[u"); // restore cursor
                    let _ = stdout.write_all(&buf);
                    let _ = stdout.flush();
                    // Shrink cached height so a later dismiss/clear_popup targets the right row.
                    layout.height -= 1;
                    self.last_layout = Some(layout);
                }
            }
            return true;
        }
        self.feedback = AsyncFeedback::Idle;
        self.dismiss(stdout);
        true
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
        self.trigger_requested = false;
        self.debounce_suppressed = false;
        if let Some(handle) = self.dynamic_task.take() {
            handle.abort();
        }
        self.dynamic_rx = None;
        self.dynamic_ctx = None;
        self.feedback = AsyncFeedback::Idle;
        self.pending_failed.clear();
        self.pending_empty_count = 0;
        // Invalidate the idempotency guard so the next trigger (e.g. after
        // ESC-then-retrigger on the same buffer) runs a fresh suggest_sync
        // instead of short-circuiting.
        self.last_trigger_fingerprint = None;
    }

    /// Compute the accept bytes for the currently-selected suggestion using
    /// an already-locked parser. Caller owns the lock so additional reads
    /// (e.g. for CD chaining prediction) can happen under the same critical
    /// section without a second `parser.lock()` round-trip.
    ///
    /// Returns `(forward_bytes, cwd, cursor_position, screen_dimensions)`:
    /// the first element is what the simple-accept path needs, the remaining
    /// three are cheap to pull from the same `TerminalState` snapshot and
    /// are consumed by `accept_with_chaining` when the selection is a
    /// directory.
    ///
    /// Returns `None` when the overlay has no valid selection.
    fn accept_suggestion_locked(&self, p: &TerminalParser) -> Option<AcceptLockedOutput> {
        let selected_idx = self.overlay.selected?;
        if selected_idx >= self.suggestions.len() {
            return None;
        }
        let selected = &self.suggestions[selected_idx];

        let state = p.state();
        let buffer = state.command_buffer().unwrap_or("");
        let cursor = state.buffer_cursor();
        let ctx = parse_command_context(buffer, cursor);
        let cwd = state.cwd().cloned().unwrap_or_else(|| PathBuf::from("."));
        let cursor_position = state.cursor_position();
        let screen_dimensions = state.screen_dimensions();

        let (delete_chars, replacement, command) =
            if selected.kind == gc_suggest::SuggestionKind::History {
                // History: delete the entire buffer up to cursor, then type
                // the full command. Cursor is always at buffer end when
                // popup is visible (arrow keys dismiss), but we use cursor
                // (not buffer.chars().count()) because over-deleting past
                // cursor into the prompt would be worse than leaving
                // trailing chars.
                //
                // Defense-in-depth: clamp cursor to buffer length even
                // though set_command_buffer already clamps, to prevent PTY
                // corruption if an unclamped value ever reaches here.
                let buf_len = buffer.chars().count();
                let safe_cursor = cursor.min(buf_len);
                if safe_cursor != buf_len {
                    tracing::warn!(
                        cursor = safe_cursor,
                        buffer_len = buf_len,
                        "history accept: cursor not at end of buffer — \
                         using cursor position to avoid over-deleting into prompt"
                    );
                }
                (safe_cursor, selected.text.clone(), ctx.command)
            } else {
                let word_len = ctx.current_word.chars().count();
                (word_len, selected.text.clone(), ctx.command)
            };

        // Record accepted completion for frecency scoring.
        // History items are full commands — always keyed without command scope
        // so the key is consistent regardless of buffer parse state.
        let frecency_command = if selected.kind == gc_suggest::SuggestionKind::History {
            None
        } else {
            command.as_deref()
        };
        self.engine
            .record_frecency(frecency_command, selected.kind, &selected.text);

        // One 0x7F (backspace) per CHARACTER — the shell deletes by character, not byte
        let mut bytes = vec![0x7F; delete_chars];
        bytes.extend_from_slice(replacement.as_bytes());

        Some((bytes, cwd, cursor_position, screen_dimensions))
    }

    fn accept_suggestion(&self, parser: &Arc<Mutex<TerminalParser>>) -> Vec<u8> {
        // Poison handling mirrors Task B in proxy.rs: if the parser
        // mutex is poisoned we can't safely read the buffer, so return
        // empty bytes (caller treats this as "no-op accept"). Failing
        // here must not crash the proxy.
        let p = match parser.lock() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("parser mutex poisoned in accept_suggestion: {e} — dropping accept");
                return Vec::new();
            }
        };
        match self.accept_suggestion_locked(&p) {
            Some((bytes, _cwd, _cursor_position, _screen_dimensions)) => bytes,
            None => Vec::new(),
        }
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

    /// Abort any in-flight dynamic generator task. Called during proxy
    /// shutdown to prevent orphaned background tasks.
    pub fn abort_dynamic_task(&mut self) {
        if let Some(handle) = self.dynamic_task.take() {
            handle.abort();
        }
        self.feedback = AsyncFeedback::Idle;
        self.pending_failed.clear();
        self.pending_empty_count = 0;
    }

    /// Abort any in-flight dynamic generator task and clear the spawn-time
    /// context snapshot. Used by the keystroke-cancel arm of the bounded-block
    /// debounce path: the rx was already dropped, so without aborting the task
    /// here it would burn CPU/IO and its eventual results would be silently
    /// discarded by `try_merge_dynamic` (rx is None).
    pub fn abort_dynamic_task_and_clear_ctx(&mut self) {
        if let Some(handle) = self.dynamic_task.take() {
            handle.abort();
        }
        self.dynamic_ctx = None;
        self.dynamic_rx = None;
        self.feedback = AsyncFeedback::Idle;
        self.pending_failed.clear();
        self.pending_empty_count = 0;
    }

    /// Flush unsaved frecency records to disk. Call on proxy shutdown.
    pub fn flush_frecency(&self) {
        self.engine.flush_frecency();
    }
}

/// Return value of `accept_suggestion_locked`: the bytes to forward to the
/// PTY plus the cwd and terminal geometry read under the same parser lock.
/// The cwd and geometry are only consumed by the CD-chaining path in
/// `accept_with_chaining`; the plain accept path discards them.
type AcceptLockedOutput = (Vec<u8>, PathBuf, (u16, u16), (u16, u16));

const DEFAULT_TRIGGER_CHARS: &[char] = &[' ', '/', '-', '.'];

/// Default generator timeout applied when [`InputHandler::with_suggest_config`]
/// is never called (primarily in tests). Production proxy always passes the
/// value resolved from `gc_config::SuggestConfig::generator_timeout_ms`.
pub const DEFAULT_GENERATOR_TIMEOUT_MS: u64 = 5000;

/// Compute a lightweight fingerprint of the current command-line buffer for
/// the trigger-idempotency guard on `InputHandler::last_trigger_fingerprint`.
/// Collision resistance doesn't need to be cryptographic — a same-content
/// match just short-circuits `trigger()` (saving work and avoiding the
/// stale-dismiss bug); a false collision would at worst miss one re-render,
/// which the next real buffer edit fixes. `DefaultHasher` on the raw bytes
/// of the buffer is sufficient.
fn buffer_fingerprint(buffer: &str, cursor: usize) -> (u64, usize) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    buffer.hash(&mut hasher);
    (hasher.finish(), cursor)
}

/// Build the env-var snapshot handed to providers as `ProviderCtx.env`.
///
/// Extracted as a pure helper so the `!provider_generators.is_empty()`
/// branching logic inside `spawn_generators`'s `tokio::spawn` block is
/// testable without standing up a full PTY event loop. The snapshot is
/// produced once per resolution pass (not per-provider) and handed
/// through as an `Arc`, which the caller wraps — this helper only owns
/// the "scan env or skip" decision.
///
/// When `has_providers` is false, returns an empty map: skips the
/// `std::env::vars().collect()` walk on the keystroke hot path for
/// script-only specs (no current provider reads `ctx.env`, so the
/// collected map would be dead weight). When true, snapshots the full
/// process env so providers observe a consistent view even if the
/// shell mutates `$PATH` between their spawns.
fn build_env_snapshot(has_providers: bool) -> std::collections::HashMap<String, String> {
    if has_providers {
        std::env::vars().collect()
    } else {
        std::collections::HashMap::new()
    }
}

/// Two borrowed `HashSet<&str>`s (existing + per-batch) — keeping references
/// rather than owned `String` keys avoids the `s.text.clone()` per dupe check
/// that the previous owned-HashSet version paid on every dynamic merge.
fn merge_dedup_against(existing: &[Suggestion], incoming: Vec<Suggestion>) -> Vec<Suggestion> {
    let keep: Vec<bool> = {
        let existing_set: HashSet<&str> = existing.iter().map(|s| s.text.as_str()).collect();
        let mut batch_seen: HashSet<&str> = HashSet::with_capacity(incoming.len());
        incoming
            .iter()
            .map(|s| !existing_set.contains(s.text.as_str()) && batch_seen.insert(s.text.as_str()))
            .collect()
    };
    incoming
        .into_iter()
        .zip(keep)
        .filter_map(|(s, k)| if k { Some(s) } else { None })
        .collect()
}

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
        KeyEvent::PageUp => vec![0x1B, b'[', b'5', b'~'],
        KeyEvent::PageDown => vec![0x1B, b'[', b'6', b'~'],
        KeyEvent::Home => vec![0x1B, b'[', b'H'],
        KeyEvent::HomeCsiTilde => vec![0x1B, b'[', b'1', b'~'],
        KeyEvent::HomeCsi7Tilde => vec![0x1B, b'[', b'7', b'~'],
        KeyEvent::HomeSs3 => vec![0x1B, b'O', b'H'],
        KeyEvent::End => vec![0x1B, b'[', b'F'],
        KeyEvent::EndCsiTilde => vec![0x1B, b'[', b'4', b'~'],
        KeyEvent::EndCsi8Tilde => vec![0x1B, b'[', b'8', b'~'],
        KeyEvent::EndSs3 => vec![0x1B, b'O', b'F'],
        KeyEvent::CtrlSpace => vec![0x00],
        KeyEvent::CtrlSlash => vec![0x1F],
        KeyEvent::Backspace => vec![0x7F],
        KeyEvent::Ctrl(c) => {
            if !c.is_ascii_lowercase() {
                tracing::error!(char = ?c, "Ctrl(char) contains non-lowercase ASCII — skipping");
                return Vec::new();
            }
            vec![*c as u8 - 0x60]
        }
        KeyEvent::Printable(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
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
    fn test_key_to_bytes_page_up_round_trip() {
        assert_eq!(key_to_bytes(&KeyEvent::PageUp), b"\x1B[5~");
    }

    #[test]
    fn test_key_to_bytes_page_down_round_trip() {
        assert_eq!(key_to_bytes(&KeyEvent::PageDown), b"\x1B[6~");
    }

    #[test]
    fn test_key_to_bytes_home_round_trip() {
        assert_eq!(key_to_bytes(&KeyEvent::Home), b"\x1B[H");
        assert_eq!(key_to_bytes(&KeyEvent::HomeCsiTilde), b"\x1B[1~");
        assert_eq!(key_to_bytes(&KeyEvent::HomeCsi7Tilde), b"\x1B[7~");
        assert_eq!(key_to_bytes(&KeyEvent::HomeSs3), b"\x1BOH");
    }

    #[test]
    fn test_key_to_bytes_end_round_trip() {
        assert_eq!(key_to_bytes(&KeyEvent::End), b"\x1B[F");
        assert_eq!(key_to_bytes(&KeyEvent::EndCsiTilde), b"\x1B[4~");
        assert_eq!(key_to_bytes(&KeyEvent::EndCsi8Tilde), b"\x1B[8~");
        assert_eq!(key_to_bytes(&KeyEvent::EndSs3), b"\x1BOF");
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
            KeyEvent::PageUp,
            KeyEvent::PageDown,
            KeyEvent::Home,
            KeyEvent::HomeCsiTilde,
            KeyEvent::HomeCsi7Tilde,
            KeyEvent::HomeSs3,
            KeyEvent::End,
            KeyEvent::EndCsiTilde,
            KeyEvent::EndCsi8Tilde,
            KeyEvent::EndSs3,
            KeyEvent::CtrlSpace,
            KeyEvent::CtrlSlash,
            KeyEvent::Backspace,
            KeyEvent::Printable('a'),
            KeyEvent::Raw(vec![0xFF]),
            KeyEvent::Ctrl('a'),
            KeyEvent::Ctrl('d'),
            KeyEvent::Ctrl('z'),
        ];
        for key in keys {
            let bytes = key_to_bytes(&key);
            assert!(!bytes.is_empty(), "key_to_bytes({:?}) was empty", key);
        }
    }

    #[test]
    fn test_key_to_bytes_ctrl() {
        assert_eq!(key_to_bytes(&KeyEvent::Ctrl('a')), vec![0x01]);
        assert_eq!(key_to_bytes(&KeyEvent::Ctrl('d')), vec![0x04]);
        assert_eq!(key_to_bytes(&KeyEvent::Ctrl('z')), vec![0x1A]);
    }

    #[test]
    fn test_try_merge_dynamic_empty_query_sorts_branches_before_history_and_files() {
        // Regression: on `git checkout <TAB>` the sync pass returns
        // [filesystem files, history], the popup paints, and then async git
        // branches arrive. Previously the empty-query branch of
        // `try_merge_dynamic` skipped sorting entirely and just `extend`-ed,
        // which left branches stranded BELOW the earlier rows. Branches must
        // sort to the top by effective priority.
        use gc_suggest::SuggestionKind;

        let mut handler = make_visible_handler(vec![
            Suggestion {
                text: "Makefile".to_string(),
                kind: SuggestionKind::FilePath,
                source: SuggestionSource::Filesystem,
                ..Default::default()
            },
            Suggestion {
                text: "git checkout demo".to_string(),
                kind: SuggestionKind::History,
                source: SuggestionSource::History,
                ..Default::default()
            },
        ]);

        // Prime the snapshot so the staleness check against a freshly-parsed
        // empty buffer passes (both ends resolve to command=None, args=[], word_index=0).
        let base_ctx = gc_buffer::parse_command_context("", 0);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));

        let (tx, rx) = mpsc::channel::<DynamicResult>(1);
        tx.try_send(DynamicResult::Loaded {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
            suggestions: vec![
                Suggestion {
                    text: "main".to_string(),
                    kind: SuggestionKind::GitBranch,
                    source: SuggestionSource::Git,
                    ..Default::default()
                },
                Suggestion {
                    text: "v1.0".to_string(),
                    kind: SuggestionKind::GitTag,
                    source: SuggestionSource::Git,
                    ..Default::default()
                },
            ],
        })
        .unwrap();
        drop(tx);
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let merged = handler.try_merge_dynamic(&parser, &mut buf);

        assert!(merged, "merge should have happened");
        let kinds: Vec<SuggestionKind> = handler.suggestions.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SuggestionKind::GitBranch,
                SuggestionKind::GitTag,
                SuggestionKind::FilePath,
                SuggestionKind::History,
            ],
            "branches and tags must land above files and history on empty query: {:?}",
            handler.suggestions,
        );
    }

    #[test]
    fn test_try_merge_dynamic_empty_query_stable_tiebreak_by_text() {
        // When two dynamic arrivals share the same effective priority (e.g. two
        // `GitBranch` entries), the comparator falls through to
        // `then_with(|| a.text.cmp(&b.text))` so the popup order is
        // alphabetic rather than channel-arrival order. Locks in both tiers
        // of the comparator: kind-priority first, text second.
        use gc_suggest::SuggestionKind;

        let mut handler = make_visible_handler(vec![Suggestion {
            text: "Makefile".to_string(),
            kind: SuggestionKind::FilePath,
            source: SuggestionSource::Filesystem,
            ..Default::default()
        }]);

        let base_ctx = gc_buffer::parse_command_context("", 0);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));

        let (tx, rx) = mpsc::channel::<DynamicResult>(1);
        tx.try_send(DynamicResult::Loaded {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
            suggestions: vec![
                Suggestion {
                    text: "zeta".to_string(),
                    kind: SuggestionKind::GitBranch,
                    source: SuggestionSource::Git,
                    ..Default::default()
                },
                Suggestion {
                    text: "alpha".to_string(),
                    kind: SuggestionKind::GitBranch,
                    source: SuggestionSource::Git,
                    ..Default::default()
                },
            ],
        })
        .unwrap();
        drop(tx);
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let merged = handler.try_merge_dynamic(&parser, &mut buf);

        assert!(merged, "merge should have happened");
        let ordered: Vec<(SuggestionKind, String)> = handler
            .suggestions
            .iter()
            .map(|s| (s.kind, s.text.clone()))
            .collect();
        assert_eq!(
            ordered,
            vec![
                (SuggestionKind::GitBranch, "alpha".to_string()),
                (SuggestionKind::GitBranch, "zeta".to_string()),
                (SuggestionKind::FilePath, "Makefile".to_string()),
            ],
            "same-priority branches must tiebreak alphabetically and both land above files: {:?}",
            handler.suggestions,
        );
    }

    #[test]
    fn test_try_merge_dynamic_keeps_open_receiver_for_later_batches() {
        let mut handler = make_visible_handler(Vec::new());
        let base_ctx = gc_buffer::parse_command_context("", 0);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));
        handler.spawned_generation = handler.buffer_generation;

        let (tx, rx) = mpsc::channel::<DynamicResult>(2);
        tx.try_send(DynamicResult::Loaded {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
            suggestions: vec![Suggestion {
                text: "main".to_string(),
                kind: SuggestionKind::GitBranch,
                source: SuggestionSource::Git,
                ..Default::default()
            }],
        })
        .unwrap();
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        assert!(handler.try_merge_dynamic(&parser, &mut buf));
        assert!(
            handler.dynamic_rx.is_some(),
            "open receiver must stay installed for later dynamic batches"
        );
        assert!(
            handler.dynamic_ctx.is_some(),
            "fresh context must survive until the dynamic channel disconnects"
        );

        tx.try_send(DynamicResult::Loaded {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Tags),
            suggestions: vec![Suggestion {
                text: "v1.0".to_string(),
                kind: SuggestionKind::GitTag,
                source: SuggestionSource::Git,
                ..Default::default()
            }],
        })
        .unwrap();
        drop(tx);

        let mut buf = Vec::new();
        assert!(handler.try_merge_dynamic(&parser, &mut buf));
        let texts: Vec<&str> = handler
            .suggestions
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(texts, vec!["main", "v1.0"]);
        assert!(
            handler.dynamic_rx.is_none(),
            "receiver should clear after the final disconnected batch"
        );
        assert!(
            handler.dynamic_ctx.is_none(),
            "context should clear after the final disconnected batch"
        );
    }

    #[test]
    fn test_try_merge_dynamic_disconnected_rerenders_to_clear_loading() {
        // Regression: when the dynamic channel disconnects (generator task
        // finished without sending, or was aborted), `try_merge_dynamic`
        // previously cleared `dynamic_rx` but did NOT re-render. The popup
        // kept showing the loading indicator from its last paint because
        // render() reads `loading = self.dynamic_rx.is_some()` — without a
        // fresh render, the on-screen indicator is a stale snapshot. On an
        // idle shell this would stay stuck until the user typed or
        // dismissed manually.
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "static".to_string(),
            ..Default::default()
        }]);

        // Closed receiver: drop tx immediately so try_recv returns
        // Disconnected on the first call.
        let (tx, rx) = mpsc::channel::<DynamicResult>(1);
        drop(tx);
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        handler.try_merge_dynamic(&parser, &mut buf);

        assert!(
            handler.dynamic_rx.is_none(),
            "dynamic_rx must be cleared on Disconnected"
        );
        assert!(
            !buf.is_empty(),
            "Disconnected path must re-render so the loading indicator clears"
        );
    }

    #[test]
    fn test_render_survives_poisoned_parser_mutex() {
        // Regression: previously render() called `parser.lock().unwrap()`,
        // which panics on poison. A poisoned parser mutex (from any prior
        // panic inside a parser lock in Task B or elsewhere) must not take
        // down Task B's render path.
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "poisoned".to_string(),
            ..Default::default()
        }]);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));

        // Poison the mutex by panicking inside a guard.
        let parser_clone = parser.clone();
        let _ = std::thread::spawn(move || {
            let _guard = parser_clone.lock().unwrap();
            panic!("intentional poison for test");
        })
        .join();
        assert!(parser.is_poisoned(), "setup: mutex must be poisoned");

        // Must not panic.
        let mut buf = Vec::new();
        handler.render(&parser, &mut buf);
    }

    #[test]
    fn test_accept_suggestion_survives_poisoned_parser_mutex() {
        // Regression: previously accept_suggestion() called
        // `parser.lock().unwrap()`, which panics on poison. Must return
        // an empty byte vec instead so the PTY proxy can continue cleanly.
        let handler = make_selected_handler(Suggestion {
            text: "poisoned".to_string(),
            kind: SuggestionKind::Subcommand,
            source: SuggestionSource::Spec,
            ..Default::default()
        });
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));

        let parser_clone = parser.clone();
        let _ = std::thread::spawn(move || {
            let _guard = parser_clone.lock().unwrap();
            panic!("intentional poison for test");
        })
        .join();
        assert!(parser.is_poisoned(), "setup: mutex must be poisoned");

        let bytes = handler.accept_suggestion(&parser);
        assert!(
            bytes.is_empty(),
            "accept_suggestion with poisoned mutex must return empty, got {bytes:?}"
        );
    }

    #[test]
    fn test_trigger_survives_poisoned_parser_mutex() {
        // Regression: previously trigger() called `parser.lock().unwrap()`,
        // which panics on poison. trigger() is the main entry point of the
        // suggestion pipeline — it runs from the debounce loop, Task B's
        // buffer_dirty/cwd_dirty branches, and the SIGWINCH handler — so a
        // poisoned parser (from any prior panic inside a parser lock) must
        // not propagate up through trigger().
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "poisoned".to_string(),
            ..Default::default()
        }]);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));

        // Poison the mutex by panicking inside a guard.
        let parser_clone = parser.clone();
        let _ = std::thread::spawn(move || {
            let _guard = parser_clone.lock().unwrap();
            panic!("intentional poison for test");
        })
        .join();
        assert!(parser.is_poisoned(), "setup: mutex must be poisoned");

        // Must not panic — trigger should log a warning and return without
        // touching the parser on the poisoned path.
        let mut buf = Vec::new();
        handler.trigger(&parser, &mut buf);
    }

    #[test]
    fn test_accept_with_chaining_survives_poisoned_parser_mutex() {
        // Regression: previously accept_with_chaining() called
        // `parser.lock().unwrap()` on the directory-chaining path, which
        // panics on poison. accept_with_chaining() runs every time the
        // user Tab-accepts a directory suggestion, so a poisoned parser
        // must not take down the proxy.
        let mut handler = make_selected_handler(Suggestion {
            // Trailing '/' makes is_dir=true, which is what hits the
            // parser.lock().unwrap() path inside accept_with_chaining.
            text: "Desktop/".to_string(),
            kind: SuggestionKind::Directory,
            source: SuggestionSource::Filesystem,
            ..Default::default()
        });
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));

        // Poison the mutex by panicking inside a guard.
        let parser_clone = parser.clone();
        let _ = std::thread::spawn(move || {
            let _guard = parser_clone.lock().unwrap();
            panic!("intentional poison for test");
        })
        .join();
        assert!(parser.is_poisoned(), "setup: mutex must be poisoned");

        // Must not panic — accept_with_chaining should log a warning and
        // return an empty byte vec so Task A forwards nothing to the PTY.
        let mut buf = Vec::new();
        let bytes = handler.accept_with_chaining(&parser, &mut buf);
        assert!(
            bytes.is_empty(),
            "accept_with_chaining with poisoned mutex must return empty, got {bytes:?}"
        );
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

    #[test]
    fn test_trigger_idempotent_when_buffer_unchanged() {
        // Scenario:
        //   1. A prior `trigger()` populated the popup with static
        //      suggestions — visible=true, last_trigger_fingerprint is
        //      set for buffer B (fingerprint stamped on successful render
        //      in the `!result.suggestions.is_empty()` arm).
        //   2. A spurious re-trigger fires with buffer still at B (e.g.
        //      debounce loop tick, or SIGWINCH / Task B re-trigger without
        //      any intervening buffer edit).
        //   3. Without the idempotency guard, `suggest_sync` re-runs. If
        //      it returns empty with no async generators, the
        //      empty-no-generators arm calls `self.dismiss(stdout)`,
        //      emitting a clear-popup ANSI sequence and tearing down the
        //      popup — it disappears for no user-driven reason.
        //
        // `trigger()` fingerprints (buffer_hash, cursor_offset) and
        // short-circuits when the fingerprint matches AND the popup is
        // still visible. ESC clears the fingerprint (via `dismiss()`),
        // and a genuine buffer edit produces a different fingerprint.
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "prior-static".to_string(),
            ..Default::default()
        }]);

        // Drive the parser to report a non-empty buffer. OSC 7770 ;
        // <cursor> ; <buffer> BEL is the shell-integration buffer report
        // consumed by `gc-parser` (see performer.rs OSC 7770 handler).
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let buffer = "xyzbogus";
        let cursor = buffer.chars().count();
        let osc = format!("\x1b]7770;{cursor};{buffer}\x07");
        parser.lock().unwrap().process_bytes(osc.as_bytes());
        assert_eq!(
            parser.lock().unwrap().state().command_buffer(),
            Some(buffer),
            "setup: OSC 7770 must land in command_buffer"
        );

        // Seed the fingerprint as if a prior trigger had populated this
        // popup for this exact buffer+cursor. This matches what the real
        // code path sets on the `!result.suggestions.is_empty()` arm.
        handler.last_trigger_fingerprint = Some(buffer_fingerprint(buffer, cursor));

        // First re-trigger: must be a full no-op (guard short-circuits
        // BEFORE suggest_sync runs, so no dismiss, no render, no writes).
        let mut buf1 = Vec::new();
        handler.trigger(&parser, &mut buf1);
        assert!(
            handler.visible,
            "popup must remain visible after idempotent re-trigger"
        );
        assert_eq!(
            handler.suggestions.len(),
            1,
            "prior static suggestion must survive idempotent re-trigger"
        );
        assert!(
            buf1.is_empty(),
            "idempotent re-trigger must not emit ANY bytes to stdout \
             (no clear-popup sequence, no re-render), got {:?}",
            String::from_utf8_lossy(&buf1)
        );

        // Second re-trigger with unchanged state: still a full no-op.
        let mut buf2 = Vec::new();
        handler.trigger(&parser, &mut buf2);
        assert!(
            handler.visible,
            "popup must remain visible after second idempotent re-trigger"
        );
        assert!(
            buf2.is_empty(),
            "second idempotent re-trigger must not emit ANY bytes, got {:?}",
            String::from_utf8_lossy(&buf2)
        );
    }

    fn make_handler() -> InputHandler {
        InputHandler {
            engine: Arc::new(SuggestionEngine::new(&[PathBuf::from(".")]).unwrap()),
            overlay: OverlayState::new(),
            suggestions: Vec::new(),
            last_layout: None,
            visible: false,
            trigger_requested: false,
            max_visible: DEFAULT_MAX_VISIBLE,
            trigger_chars: DEFAULT_TRIGGER_CHARS.to_vec(),
            debounce_suppressed: false,
            auto_trigger: true,
            keybindings: Keybindings::default(),
            theme: PopupTheme::default(),
            generator_timeout_ms: DEFAULT_GENERATOR_TIMEOUT_MS,
            dynamic_rx: None,
            dynamic_task: None,
            dynamic_notify: Arc::new(Notify::new()),
            feedback_tick_notify: Arc::new(Notify::new()),
            feedback: AsyncFeedback::Idle,
            feedback_dismiss_ms: 1200,
            pending_failed: Vec::new(),
            pending_empty_count: 0,
            dynamic_ctx: None,
            terminal_profile: TerminalProfile::for_ghostty(),
            scroll_deficit: 0,
            last_trigger_fingerprint: None,
            buffer_generation: 0,
            spawned_generation: 0,
            render_block_ms: 0,
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

    fn numbered_suggestions(count: usize) -> Vec<Suggestion> {
        (0..count)
            .map(|n| Suggestion {
                text: format!("item-{n}"),
                kind: SuggestionKind::Command,
                source: SuggestionSource::Spec,
                ..Default::default()
            })
            .collect()
    }

    /// Test builder: visible handler with a single selected suggestion.
    fn make_selected_handler(suggestion: Suggestion) -> InputHandler {
        let mut h = make_visible_handler(vec![suggestion]);
        h.overlay.selected = Some(0);
        h
    }

    #[test]
    fn test_page_down_when_visible_advances_selection() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.overlay.selected = Some(5);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::PageDown, &parser, &mut buf);

        assert!(result.is_empty());
        assert_eq!(handler.overlay.selected, Some(15));
    }

    #[test]
    fn test_page_down_uses_effective_popup_height_on_short_terminal() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.overlay.selected = Some(5);
        handler.overlay.scroll_offset = 0;
        handler.last_layout = Some(PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 20,
            height: 3,
            scroll_deficit: 0,
        });
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(4, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::PageDown, &parser, &mut buf);

        assert!(result.is_empty());
        assert_eq!(handler.overlay.selected, Some(8));
        assert_eq!(handler.overlay.scroll_offset, 6);
        let selected = handler.overlay.selected.unwrap();
        assert!(handler.overlay.scroll_offset <= selected);
        assert!(selected < handler.overlay.scroll_offset + 3);
    }

    #[test]
    fn test_page_up_uses_effective_popup_height_on_short_terminal() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.overlay.selected = Some(8);
        handler.overlay.scroll_offset = 6;
        handler.last_layout = Some(PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 20,
            height: 3,
            scroll_deficit: 0,
        });
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(4, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::PageUp, &parser, &mut buf);

        assert!(result.is_empty());
        assert_eq!(handler.overlay.selected, Some(5));
        assert_eq!(handler.overlay.scroll_offset, 5);
        let selected = handler.overlay.selected.unwrap();
        assert!(handler.overlay.scroll_offset <= selected);
        assert!(selected < handler.overlay.scroll_offset + 3);
    }

    #[test]
    fn test_end_uses_effective_popup_height_with_bordered_short_terminal() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.theme = PopupTheme {
            borders: true,
            ..PopupTheme::default()
        };
        handler.last_layout = Some(PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 20,
            height: 5,
            scroll_deficit: 0,
        });
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(6, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::End, &parser, &mut buf);

        assert!(result.is_empty());
        assert_eq!(handler.overlay.selected, Some(49));
        assert_eq!(handler.overlay.scroll_offset, 47);
        let selected = handler.overlay.selected.unwrap();
        assert!(handler.overlay.scroll_offset <= selected);
        assert!(selected < handler.overlay.scroll_offset + 3);
    }

    #[test]
    fn test_page_down_uses_configured_height_when_borderless_popup_suppressed() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.overlay.selected = Some(5);
        handler.overlay.scroll_offset = 0;
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(1, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::PageDown, &parser, &mut buf);

        assert!(result.is_empty());
        assert!(handler.visible);
        assert_eq!(handler.overlay.selected, Some(15));
        assert_eq!(handler.overlay.scroll_offset, 6);
    }

    #[test]
    fn test_end_uses_configured_height_when_bordered_popup_suppressed() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.theme = PopupTheme {
            borders: true,
            ..PopupTheme::default()
        };
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(3, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::End, &parser, &mut buf);

        assert!(result.is_empty());
        assert!(handler.visible);
        assert_eq!(handler.overlay.selected, Some(49));
        assert_eq!(handler.overlay.scroll_offset, 40);
    }

    #[test]
    fn test_page_up_when_visible_retreats_selection() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.overlay.selected = Some(15);
        handler.overlay.scroll_offset = 6;
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::PageUp, &parser, &mut buf);

        assert!(result.is_empty());
        assert_eq!(handler.overlay.selected, Some(5));
    }

    #[test]
    fn test_home_when_visible_jumps_to_zero() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.overlay.selected = Some(20);
        handler.overlay.scroll_offset = 11;
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::Home, &parser, &mut buf);

        assert!(result.is_empty());
        assert_eq!(handler.overlay.selected, Some(0));
        assert_eq!(handler.overlay.scroll_offset, 0);
    }

    #[test]
    fn test_end_when_visible_jumps_to_last() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::End, &parser, &mut buf);

        assert!(result.is_empty());
        assert_eq!(handler.overlay.selected, Some(49));
        assert_eq!(handler.overlay.scroll_offset, 40);
    }

    #[test]
    fn test_page_down_intercepted_when_visible_returns_empty_bytes() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::PageDown, &parser, &mut buf);

        assert!(result.is_empty());
    }

    #[test]
    fn test_split_page_up_sequence_is_buffered_until_visible_popup_can_intercept() {
        let mut key_parser = crate::input::KeyParser::new();
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.overlay.selected = Some(15);
        handler.overlay.scroll_offset = 6;
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let first_keys = key_parser.parse(b"\x1B[5");
        assert!(first_keys.is_empty());
        assert!(handler.visible);

        let second_keys = key_parser.parse(b"~");
        assert_eq!(second_keys, vec![KeyEvent::PageUp]);
        let result = handler.process_key(&second_keys[0], &parser, &mut buf);

        assert!(result.is_empty());
        assert!(handler.visible);
        assert_eq!(handler.overlay.selected, Some(5));
    }

    #[test]
    fn test_page_down_when_hidden_forwards_bytes() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::PageDown, &parser, &mut buf);

        assert_eq!(result, b"\x1B[6~");
    }

    #[test]
    fn test_page_up_when_hidden_forwards_bytes() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::PageUp, &parser, &mut buf);

        assert_eq!(result, b"\x1B[5~");
    }

    #[test]
    fn test_home_when_hidden_forwards_bytes() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::Home, &parser, &mut buf);

        assert_eq!(result, b"\x1B[H");
    }

    #[test]
    fn test_end_when_hidden_forwards_bytes() {
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let result = handler.process_key(&KeyEvent::End, &parser, &mut buf);

        assert_eq!(result, b"\x1B[F");
    }

    #[test]
    fn test_hidden_home_end_alternate_encodings_forward_verbatim() {
        for raw in [
            b"\x1B[1~".as_slice(),
            b"\x1B[4~",
            b"\x1B[7~",
            b"\x1B[8~",
            b"\x1BOH",
            b"\x1BOF",
        ] {
            let events = crate::input::parse_keys(raw);
            assert_eq!(events.len(), 1, "expected one parsed event for {raw:?}");

            let mut handler = make_handler();
            let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
            let mut buf = Vec::new();

            let result = handler.process_key(&events[0], &parser, &mut buf);

            assert_eq!(result, raw, "hidden popup must forward {raw:?} unchanged");
        }
    }

    #[test]
    fn test_visible_home_end_csi_7_8_tilde_navigate() {
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let mut home_handler = make_visible_handler(numbered_suggestions(50));
        home_handler.overlay.selected = Some(20);
        home_handler.overlay.scroll_offset = 11;
        let home_result = home_handler.process_key(&KeyEvent::HomeCsi7Tilde, &parser, &mut buf);

        assert!(home_result.is_empty());
        assert!(home_handler.visible);
        assert_eq!(home_handler.overlay.selected, Some(0));
        assert_eq!(home_handler.overlay.scroll_offset, 0);

        let mut end_handler = make_visible_handler(numbered_suggestions(50));
        let end_result = end_handler.process_key(&KeyEvent::EndCsi8Tilde, &parser, &mut buf);

        assert!(end_result.is_empty());
        assert!(end_handler.visible);
        assert_eq!(end_handler.overlay.selected, Some(49));
        assert_eq!(end_handler.overlay.scroll_offset, 40);
    }

    #[test]
    fn test_visible_home_variants_jump_to_zero() {
        for key in [
            KeyEvent::Home,
            KeyEvent::HomeCsiTilde,
            KeyEvent::HomeCsi7Tilde,
            KeyEvent::HomeSs3,
        ] {
            let mut handler = make_visible_handler(numbered_suggestions(50));
            handler.overlay.selected = Some(20);
            handler.overlay.scroll_offset = 11;
            let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
            let mut buf = Vec::new();

            let result = handler.process_key(&key, &parser, &mut buf);

            assert!(result.is_empty(), "{key:?} should be intercepted");
            assert!(handler.visible, "{key:?} should not dismiss popup");
            assert_eq!(handler.overlay.selected, Some(0), "{key:?}");
            assert_eq!(handler.overlay.scroll_offset, 0, "{key:?}");
        }
    }

    #[test]
    fn test_visible_end_variants_jump_to_last() {
        for key in [
            KeyEvent::End,
            KeyEvent::EndCsiTilde,
            KeyEvent::EndCsi8Tilde,
            KeyEvent::EndSs3,
        ] {
            let mut handler = make_visible_handler(numbered_suggestions(50));
            let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
            let mut buf = Vec::new();

            let result = handler.process_key(&key, &parser, &mut buf);

            assert!(result.is_empty(), "{key:?} should be intercepted");
            assert!(handler.visible, "{key:?} should not dismiss popup");
            assert_eq!(handler.overlay.selected, Some(49), "{key:?}");
            assert_eq!(handler.overlay.scroll_offset, 40, "{key:?}");
        }
    }

    #[test]
    fn test_page_navigation_does_not_dismiss_popup() {
        for key in [
            KeyEvent::PageUp,
            KeyEvent::PageDown,
            KeyEvent::Home,
            KeyEvent::HomeCsiTilde,
            KeyEvent::HomeCsi7Tilde,
            KeyEvent::HomeSs3,
            KeyEvent::End,
            KeyEvent::EndCsiTilde,
            KeyEvent::EndCsi8Tilde,
            KeyEvent::EndSs3,
        ] {
            let mut handler = make_visible_handler(numbered_suggestions(50));
            handler.overlay.selected = Some(5);
            let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
            let mut buf = Vec::new();

            handler.process_key(&key, &parser, &mut buf);

            assert!(handler.visible, "{key:?} should not dismiss popup");
        }
    }

    #[test]
    fn test_page_down_then_accept_uses_new_selection() {
        let mut handler = make_visible_handler(numbered_suggestions(50));
        handler.overlay.selected = Some(5);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();

        let page_result = handler.process_key(&KeyEvent::PageDown, &parser, &mut buf);
        assert!(page_result.is_empty());

        let accept_result = handler.process_key(&KeyEvent::Tab, &parser, &mut buf);
        let accepted = String::from_utf8_lossy(&accept_result);

        assert!(
            accepted.contains("item-15"),
            "expected accept to use paged selection, got {accepted:?}"
        );
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
        // Accessing the private field directly — the public `is_visible()`
        // accessor was removed as dead API.
        assert!(!handler.visible);
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
        assert!(parse_key_name("").is_err());
        assert!(parse_key_name("banana").is_err());
        assert!(parse_key_name("ctrl+1").is_err());
        assert!(parse_key_name("ctrl+").is_err());
    }

    #[test]
    fn test_parse_key_name_ctrl_letters() {
        assert_eq!(parse_key_name("ctrl+a").unwrap(), KeyEvent::Ctrl('a'));
        assert_eq!(parse_key_name("ctrl+e").unwrap(), KeyEvent::Ctrl('e'));
        assert_eq!(parse_key_name("ctrl+n").unwrap(), KeyEvent::Ctrl('n'));
        assert_eq!(parse_key_name("ctrl+p").unwrap(), KeyEvent::Ctrl('p'));
        assert_eq!(parse_key_name("Ctrl+X").unwrap(), KeyEvent::Ctrl('x'));
    }

    #[test]
    fn test_parse_key_name_rejects_signal_keys() {
        assert!(parse_key_name("ctrl+c").is_err());
        assert!(parse_key_name("ctrl+d").is_err());
        assert!(parse_key_name("ctrl+z").is_err());
        assert!(parse_key_name("ctrl+s").is_err());
        assert!(parse_key_name("ctrl+q").is_err());
        // Case-insensitive: uppercase input hits same deny-list
        assert!(parse_key_name("CTRL+C").is_err());
        assert!(parse_key_name("Ctrl+Z").is_err());
    }

    #[test]
    fn test_parse_key_name_rejects_aliased_keys() {
        assert!(parse_key_name("ctrl+i").is_err());
        assert!(parse_key_name("ctrl+m").is_err());
        assert!(parse_key_name("CTRL+I").is_err());
    }

    #[test]
    fn test_parse_key_name_ctrl_multi_char_error() {
        let err = parse_key_name("ctrl+ab").unwrap_err();
        assert!(
            err.to_string().contains("single letter"),
            "should mention 'single letter': {err}"
        );
        let err = parse_key_name("ctrl+1").unwrap_err();
        assert!(
            err.to_string().contains("single letter"),
            "should mention 'single letter' for digits: {err}"
        );
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
            feedback_loading_on: vec![0x1B, b'[', b'2', b'm'],
            feedback_empty_on: vec![0x1B, b'[', b'2', b'm'],
            feedback_error_on: vec![0x1B, b'[', b'3', b'1', b'm'],
            match_highlight_on: vec![0x1B, b'[', b'4', b'm'],
            item_text_on: vec![],
            scrollbar_on: vec![0x1B, b'[', b'2', b'm'],
            border_on: vec![0x1B, b'[', b'2', b'm'],
            borders: true,
            spinner: true,
            show_provider_errors: false,
        };

        handler.update_config(
            new_theme,
            Keybindings::default(),
            &[' ', '/'],
            15,
            1200,
            true,
        );

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

        handler.update_config(
            PopupTheme::default(),
            new_kb.clone(),
            &[' ', '/'],
            10,
            1200,
            true,
        );

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
            1200,
            true,
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
            1200,
            true,
        );

        assert_eq!(handler.trigger_chars, vec!['@', '#', '!']);
    }

    // --- auto_trigger tests ---

    #[test]
    fn test_auto_trigger_false_suppresses_trigger_on_space() {
        let mut handler = make_handler().with_auto_trigger(false);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::Printable(' '), &parser, &mut buf);
        assert!(!handler.has_pending_trigger());
    }

    #[test]
    fn test_auto_trigger_false_allows_manual_trigger() {
        let mut handler = make_handler().with_auto_trigger(false);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.process_key(&KeyEvent::CtrlSlash, &parser, &mut buf);
        // Manual trigger fires immediately — not gated by auto_trigger
        assert!(!handler.has_pending_trigger());
    }

    #[test]
    fn test_auto_trigger_false_suppresses_trigger_on_all_auto_chars() {
        let mut handler = make_handler().with_auto_trigger(false);
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        for c in [' ', '/', '-', '.'] {
            handler.process_key(&KeyEvent::Printable(c), &parser, &mut buf);
            assert!(
                !handler.has_pending_trigger(),
                "auto_trigger=false should suppress trigger on '{c}'"
            );
        }
    }

    #[test]
    fn test_update_config_sets_auto_trigger_false() {
        let mut handler = make_handler();
        assert!(handler.auto_trigger_enabled());

        handler.update_config(
            PopupTheme::default(),
            Keybindings::default(),
            &[' ', '/', '-', '.'],
            10,
            1200,
            false,
        );

        assert!(!handler.auto_trigger_enabled());
    }

    #[test]
    fn test_update_config_dismisses_popup_on_auto_trigger_disable() {
        let suggestion = Suggestion {
            text: "test".into(),
            ..Default::default()
        };
        let mut handler = make_visible_handler(vec![suggestion]);
        assert!(handler.visible);
        assert!(handler.auto_trigger_enabled());

        let cleanup = handler.update_config(
            PopupTheme::default(),
            Keybindings::default(),
            &[' ', '/', '-', '.'],
            10,
            1200,
            false,
        );

        assert!(!handler.visible);
        assert!(!handler.auto_trigger_enabled());
        assert!(!cleanup.is_empty(), "should emit popup clear sequences");
        assert!(handler.dynamic_rx.is_none(), "dynamic_rx must be cleared");
        assert!(handler.dynamic_ctx.is_none(), "dynamic_ctx must be cleared");
        assert!(
            handler.dynamic_task.is_none(),
            "dynamic_task must be cleared"
        );
    }

    #[test]
    fn test_update_config_clears_pending_trigger_even_when_not_visible() {
        let mut handler = make_handler();
        // Simulate a pending trigger (debounce timer fired, trigger() hasn't
        // run yet) while the popup is NOT visible.
        handler.trigger_requested = true;
        assert!(!handler.visible);

        let cleanup = handler.update_config(
            PopupTheme::default(),
            Keybindings::default(),
            &[' ', '/', '-', '.'],
            10,
            1200,
            false,
        );

        assert!(
            !handler.has_pending_trigger(),
            "pending trigger must be cancelled"
        );
        assert!(handler.dynamic_task.is_none());
        assert!(handler.dynamic_rx.is_none());
        assert!(handler.dynamic_ctx.is_none());
        // No popup was visible, so no visual cleanup needed.
        assert!(cleanup.is_empty());
    }

    #[test]
    fn test_update_config_keeps_popup_when_auto_trigger_stays_true() {
        let suggestion = Suggestion {
            text: "test".into(),
            ..Default::default()
        };
        let mut handler = make_visible_handler(vec![suggestion]);
        assert!(handler.visible);

        let cleanup = handler.update_config(
            PopupTheme::default(),
            Keybindings::default(),
            &[' ', '/', '-', '.'],
            10,
            1200,
            true,
        );

        assert!(handler.visible);
        assert!(cleanup.is_empty(), "no cleanup when auto_trigger unchanged");
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

    // --- DynamicCtxSnapshot staleness truth table ---

    /// Test-only helper: build a `CommandContext` with the minimum field
    /// set the staleness tests care about. Everything else defaults to the
    /// "unquoted, first segment, not a flag" configuration.
    fn ctx(
        cmd: &str,
        args: &[&str],
        preceding_flag: Option<&str>,
        word_idx: usize,
        current_word: &str,
    ) -> gc_buffer::CommandContext {
        gc_buffer::CommandContext {
            command: Some(cmd.to_string()),
            args: args.iter().map(|s| s.to_string()).collect(),
            current_word: current_word.to_string(),
            word_index: word_idx,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: preceding_flag.map(|s| s.to_string()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
            is_first_segment: true,
        }
    }

    #[test]
    fn dynamic_ctx_identical_context_is_not_stale() {
        let base = ctx("git", &["checkout"], None, 2, "");
        let snap = DynamicCtxSnapshot::capture(&base, false);
        assert!(
            !snap.is_stale_against(&base),
            "identical context must not be stale"
        );
    }

    #[test]
    fn dynamic_ctx_different_command_is_stale() {
        let base = ctx("git", &["checkout"], None, 2, "");
        let snap = DynamicCtxSnapshot::capture(&base, false);
        let changed = ctx("docker", &["checkout"], None, 2, "");
        assert!(
            snap.is_stale_against(&changed),
            "different command must be stale"
        );
    }

    #[test]
    fn dynamic_ctx_different_args_is_stale() {
        let base = ctx("git", &["checkout"], None, 2, "");
        let snap = DynamicCtxSnapshot::capture(&base, false);
        let changed = ctx("git", &["branch"], None, 2, "");
        assert!(
            snap.is_stale_against(&changed),
            "different args must be stale"
        );
    }

    #[test]
    fn dynamic_ctx_different_preceding_flag_is_stale() {
        let base = ctx("git", &["checkout"], None, 2, "");
        let snap = DynamicCtxSnapshot::capture(&base, false);
        let changed = ctx("git", &["checkout"], Some("-b"), 2, "");
        assert!(
            snap.is_stale_against(&changed),
            "different preceding_flag must be stale"
        );
    }

    #[test]
    fn dynamic_ctx_different_word_index_is_stale() {
        let base = ctx("git", &["checkout"], None, 2, "");
        let snap = DynamicCtxSnapshot::capture(&base, false);
        let changed = ctx("git", &["checkout"], None, 3, "");
        assert!(
            snap.is_stale_against(&changed),
            "different word_index must be stale"
        );
    }

    #[test]
    fn dynamic_ctx_spawned_word_unchanged_is_not_stale() {
        // script_template generator: spawn captured current_word,
        // current context still has the same current_word.
        let base = ctx("docker", &["inspect"], None, 2, "ar");
        let snap = DynamicCtxSnapshot::capture(&base, true);
        let same_word = ctx("docker", &["inspect"], None, 2, "ar");
        assert!(
            !snap.is_stale_against(&same_word),
            "unchanged spawned current_word must not be stale"
        );
    }

    #[test]
    fn dynamic_ctx_spawned_word_changed_is_stale() {
        // The `docker inspect ar` vs `docker inspect arg` case: script
        // template substitutes `{current_token}` literally, so each
        // invocation produces a disjoint result set.
        let base = ctx("docker", &["inspect"], None, 2, "ar");
        let snap = DynamicCtxSnapshot::capture(&base, true);
        let extended_word = ctx("docker", &["inspect"], None, 2, "arg");
        assert!(
            snap.is_stale_against(&extended_word),
            "changed spawned current_word must be stale"
        );
    }

    #[test]
    fn dynamic_ctx_no_spawned_word_prefix_extension_allowed() {
        // Non-script-template generators (git branches, fuzzy filters):
        // capture with `uses_current_word = false`, so typing more
        // characters of the prefix is not a staleness trigger.
        let base = ctx("git", &["checkout"], None, 2, "ma");
        let snap = DynamicCtxSnapshot::capture(&base, false);
        let extended = ctx("git", &["checkout"], None, 2, "main");
        assert!(
            !snap.is_stale_against(&extended),
            "prefix extension must not be stale when no generator depends on current_word"
        );
    }

    #[test]
    fn dynamic_ctx_capture_with_uses_current_word_true_captures_word() {
        let c = ctx("docker", &["inspect"], None, 2, "ar");
        let snap = DynamicCtxSnapshot::capture(&c, true);
        assert_eq!(snap.spawned_current_word, Some("ar".to_string()));
    }

    #[test]
    fn dynamic_ctx_capture_with_uses_current_word_false_no_word() {
        let c = ctx("git", &["checkout"], None, 2, "ma");
        let snap = DynamicCtxSnapshot::capture(&c, false);
        assert!(snap.spawned_current_word.is_none());
    }

    // --- dismiss/trigger dynamic_task abort verification ---

    #[tokio::test]
    async fn test_dismiss_clears_dynamic_task_and_rx() {
        // Regression: dismiss() must abort any in-flight generator task
        // AND clear dynamic_rx/dynamic_ctx so a subsequent trigger can
        // start fresh without merging stale results.
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "test".to_string(),
            ..Default::default()
        }]);

        // Populate dynamic state as if generators were in flight.
        let (_tx, rx) = mpsc::channel::<DynamicResult>(1);
        handler.dynamic_rx = Some(rx);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(
            &ctx("git", &["checkout"], None, 2, ""),
            false,
        ));
        handler.dynamic_task = Some(tokio::spawn(async {
            // Long-running task that must be aborted.
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }));

        let mut stdout_buf = Vec::new();
        handler.dismiss(&mut stdout_buf);

        assert!(
            handler.dynamic_task.is_none(),
            "dismiss must clear dynamic_task"
        );
        assert!(
            handler.dynamic_rx.is_none(),
            "dismiss must clear dynamic_rx"
        );
        assert!(
            handler.dynamic_ctx.is_none(),
            "dismiss must clear dynamic_ctx"
        );
    }

    #[tokio::test]
    async fn test_trigger_aborts_in_flight_generators() {
        // Regression: when trigger() fires with a new context, any
        // in-flight generator task from a previous trigger must be
        // aborted and dynamic_rx/dynamic_ctx cleared before the new
        // generators are spawned. Otherwise stale generator results
        // could be merged into an unrelated completion site.
        let mut handler = make_handler();
        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));

        // Set buffer state so trigger() doesn't early-return on empty.
        {
            let mut p = parser.lock().unwrap();
            p.state_mut().predict_command_buffer("git ".to_string(), 4);
        }

        // Populate in-flight dynamic state mimicking a prior trigger that
        // spawned generators against a different command.
        let (_tx, rx) = mpsc::channel::<DynamicResult>(1);
        handler.dynamic_rx = Some(rx);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(
            &ctx("old-cmd", &[], None, 0, ""),
            false,
        ));
        handler.dynamic_task = Some(tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }));

        let mut stdout = Vec::new();
        handler.trigger(&parser, &mut stdout);

        // trigger() may re-populate dynamic_rx/ctx/task if the new buffer
        // produced new async generators. What matters is that the OLD
        // values were replaced, not their specific new state.
        if let Some(ref snapshot) = handler.dynamic_ctx {
            assert_ne!(
                snapshot.command.as_deref(),
                Some("old-cmd"),
                "trigger() must clear or replace stale dynamic_ctx"
            );
        }
    }

    #[tokio::test]
    async fn test_trigger_static_with_async_reserves_indicator_row_in_cached_layout() {
        let specs_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
        if !specs_dir.exists() {
            return;
        }
        let engine = SuggestionEngine::new(&[specs_dir]).unwrap();
        let mut handler = make_handler();
        handler.engine = Arc::new(engine);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        {
            let mut p = parser.lock().unwrap();
            p.state_mut()
                .predict_command_buffer("git checkout ".to_string(), 13);
        }

        let mut stdout = Vec::new();
        handler.trigger(&parser, &mut stdout);

        let layout = handler
            .last_layout
            .clone()
            .expect("trigger must paint a layout when static suggestions are present");
        assert!(
            handler.feedback.is_loading(),
            "spawn_generators must run before render_at so feedback=Loading at paint time"
        );
        let visible_count = handler.suggestions.len().min(handler.max_visible) as u16;
        assert!(
            visible_count >= 1,
            "git checkout must yield static suggestions"
        );
        assert_eq!(
            layout.height,
            visible_count + 1,
            "cached layout must reserve the indicator row (visible={visible_count}, height={})",
            layout.height,
        );
    }

    #[tokio::test]
    async fn test_abort_dynamic_task_and_clear_ctx_clears_both_fields() {
        let mut handler = make_handler();
        handler.dynamic_task = Some(tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }));
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(
            &ctx("git", &["checkout"], None, 2, ""),
            false,
        ));

        handler.abort_dynamic_task_and_clear_ctx();

        assert!(handler.dynamic_task.is_none());
        assert!(handler.dynamic_ctx.is_none());
    }

    #[test]
    fn build_env_snapshot_empty_when_no_providers() {
        // When no provider is scheduled this pass, the caller pays
        // zero cost for an env walk we don't need — pins the hot-path
        // optimization. A future refactor that collapsed the branch to
        // `std::env::vars().collect()` would flip this to non-empty on
        // every CI host (PATH is always set).
        let snapshot = build_env_snapshot(false);
        assert!(
            snapshot.is_empty(),
            "expected empty map when has_providers=false, got {} entries",
            snapshot.len()
        );
    }

    #[test]
    fn build_env_snapshot_populated_when_providers_present() {
        // When at least one provider is scheduled, the snapshot must
        // carry the real process env. PATH is the sentinel key — it is
        // set on every CI host we target (macOS and Linux runners
        // both) and is part of the default shell environment. A future
        // refactor that collapsed the branch to always-empty would
        // break every provider that reads `ctx.env`.
        let snapshot = build_env_snapshot(true);
        assert!(
            snapshot.contains_key("PATH"),
            "expected PATH in env snapshot when has_providers=true; \
             snapshot had {} entries",
            snapshot.len()
        );
    }

    fn dedup_suggestion(text: &str) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn merge_dedup_against_drops_text_duplicates_from_existing_pool() {
        let existing = vec![dedup_suggestion("main")];
        let incoming = vec![dedup_suggestion("main"), dedup_suggestion("dev")];
        let kept = merge_dedup_against(&existing, incoming);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "dev");
    }

    #[test]
    fn merge_dedup_against_drops_duplicates_within_same_batch() {
        let incoming = vec![
            dedup_suggestion("main"),
            dedup_suggestion("main"),
            dedup_suggestion("dev"),
        ];
        let kept = merge_dedup_against(&[], incoming);
        let texts: Vec<&str> = kept.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["main", "dev"]);
    }

    // --- clear_expired_feedback state machine ---

    #[test]
    fn test_clear_expired_feedback_returns_false_when_idle() {
        let mut handler = make_visible_handler(Vec::new()).with_feedback_dismiss_ms(1200);
        handler.feedback = AsyncFeedback::Idle;
        let mut buf: Vec<u8> = Vec::new();
        assert!(!handler.clear_expired_feedback(&mut buf));
        assert!(buf.is_empty());
    }

    #[test]
    fn test_clear_expired_feedback_returns_false_when_loading() {
        let mut handler = make_visible_handler(Vec::new()).with_feedback_dismiss_ms(1200);
        handler.feedback = AsyncFeedback::Loading {
            spawned_at: std::time::Instant::now(),
        };
        let mut buf: Vec<u8> = Vec::new();
        assert!(!handler.clear_expired_feedback(&mut buf));
        assert!(buf.is_empty());
    }

    #[test]
    fn test_clear_expired_feedback_returns_false_when_dismiss_disabled() {
        let mut handler = make_visible_handler(Vec::new()).with_feedback_dismiss_ms(0);
        handler.feedback = AsyncFeedback::Empty {
            since: std::time::Instant::now() - std::time::Duration::from_secs(10),
        };
        let mut buf: Vec<u8> = Vec::new();
        assert!(!handler.clear_expired_feedback(&mut buf));
        assert!(buf.is_empty());
    }

    #[test]
    fn test_clear_expired_feedback_returns_false_when_not_yet_expired() {
        let mut handler = make_visible_handler(Vec::new()).with_feedback_dismiss_ms(10_000);
        handler.feedback = AsyncFeedback::Empty {
            since: std::time::Instant::now(),
        };
        let mut buf: Vec<u8> = Vec::new();
        assert!(!handler.clear_expired_feedback(&mut buf));
        assert!(handler.visible);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_clear_expired_feedback_dismisses_on_expired_empty() {
        let mut handler = make_visible_handler(Vec::new()).with_feedback_dismiss_ms(1200);
        handler.feedback = AsyncFeedback::Empty {
            since: std::time::Instant::now() - std::time::Duration::from_millis(2000),
        };
        let mut buf: Vec<u8> = Vec::new();
        assert!(handler.clear_expired_feedback(&mut buf));
        assert!(!handler.visible);
        assert!(matches!(handler.feedback, AsyncFeedback::Idle));
    }

    #[test]
    fn test_clear_expired_feedback_partial_error_with_suggestions_demotes_to_idle() {
        // Regression: PartialError expiry with merged suggestions must keep popup visible.
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "main".into(),
            kind: SuggestionKind::GitBranch,
            ..Default::default()
        }])
        .with_feedback_dismiss_ms(1200);
        handler.feedback = AsyncFeedback::PartialError {
            failed: vec!["git script".into()],
            since: std::time::Instant::now() - std::time::Duration::from_millis(2000),
        };
        let mut buf: Vec<u8> = Vec::new();
        assert!(handler.clear_expired_feedback(&mut buf));
        assert!(handler.visible, "popup must stay visible");
        assert_eq!(handler.suggestions.len(), 1, "suggestions must survive");
        assert!(matches!(handler.feedback, AsyncFeedback::Idle));
    }

    #[test]
    fn test_clear_expired_feedback_bordered_partial_error_paints_displaced_border_row() {
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "main".into(),
            kind: SuggestionKind::GitBranch,
            ..Default::default()
        }])
        .with_feedback_dismiss_ms(1200);
        handler.theme = PopupTheme {
            borders: true,
            ..PopupTheme::default()
        };
        handler.last_layout = Some(PopupLayout {
            start_row: 5,
            start_col: 10,
            width: 20,
            height: 4,
            scroll_deficit: 0,
        });
        handler.feedback = AsyncFeedback::PartialError {
            failed: vec!["git script".into()],
            since: std::time::Instant::now() - std::time::Duration::from_millis(2000),
        };

        let mut buf: Vec<u8> = Vec::new();
        assert!(handler.clear_expired_feedback(&mut buf));
        assert!(matches!(handler.feedback, AsyncFeedback::Idle));

        let painted = String::from_utf8_lossy(&buf).into_owned();
        assert!(
            painted.contains("\x1b[8;11H"),
            "indicator row must be addressed: {painted:?}"
        );
        assert!(
            painted.contains("\x1b[9;11H"),
            "displaced bottom-border row must be cleared: {painted:?}"
        );
        // A new bottom border must be drawn at the shrunk-bottom position.
        assert!(
            painted.contains('╰') && painted.contains('╯'),
            "bottom border must be redrawn at the new position: {painted:?}"
        );

        let shrunk = handler.last_layout.clone().expect("layout retained");
        assert_eq!(shrunk.height, 3);
        let mut clear_buf: Vec<u8> = Vec::new();
        clear_popup(&mut clear_buf, &shrunk, &handler.terminal_profile);
        let clear_text = String::from_utf8_lossy(&clear_buf).into_owned();
        // Rows 5,6,7 must be addressed by clear_popup (1-based 6,7,8).
        for row_1based in [6_u16, 7, 8] {
            let needle = format!("\x1b[{row_1based};11H");
            assert!(
                clear_text.contains(&needle),
                "clear_popup must address row {row_1based}: {clear_text:?}"
            );
        }
    }

    #[test]
    fn test_pending_failed_accumulates_across_two_try_merge_dynamic_calls() {
        // Cross-batch accumulation: an Error in batch 1 must survive into
        // the disconnect-time terminal feedback computed in batch 2.
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "--flag".into(),
            kind: SuggestionKind::Flag,
            source: SuggestionSource::Spec,
            ..Default::default()
        }]);
        let base_ctx = gc_buffer::parse_command_context("", 0);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));
        handler.feedback = AsyncFeedback::Loading {
            spawned_at: std::time::Instant::now(),
        };

        let (tx, rx) = mpsc::channel::<DynamicResult>(2);
        // Batch 1: send Error only, do NOT drop tx — channel is still open.
        tx.try_send(DynamicResult::Error {
            provider: ProviderTag::Script("npm".into()),
            message: "oops".into(),
        })
        .unwrap();
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.try_merge_dynamic(&parser, &mut buf);

        // After batch 1: pending_failed has the npm error, feedback still Loading.
        assert_eq!(handler.pending_failed.len(), 1);
        assert!(handler.feedback.is_loading());

        // Batch 2: send Loaded then drop tx so the channel disconnects.
        tx.try_send(DynamicResult::Loaded {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
            suggestions: vec![Suggestion {
                text: "main".into(),
                kind: SuggestionKind::GitBranch,
                source: SuggestionSource::Git,
                ..Default::default()
            }],
        })
        .unwrap();
        drop(tx);
        handler.try_merge_dynamic(&parser, &mut buf);

        // After batch 2 + disconnect: PartialError with the npm error from
        // batch 1 must have survived.
        match handler.feedback_kind() {
            AsyncFeedback::PartialError { failed, .. } => {
                assert_eq!(failed.len(), 1, "batch-1 error must survive batch-2");
            }
            other => panic!("expected PartialError feedback, got {other:?}"),
        }
    }

    #[test]
    fn test_pending_empty_count_accumulates_across_two_try_merge_dynamic_calls() {
        // Symmetric variant of the cross-batch accumulation test, for empty.
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "--flag".into(),
            kind: SuggestionKind::Flag,
            source: SuggestionSource::Spec,
            ..Default::default()
        }]);
        let base_ctx = gc_buffer::parse_command_context("", 0);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));
        handler.feedback = AsyncFeedback::Loading {
            spawned_at: std::time::Instant::now(),
        };

        let (tx, rx) = mpsc::channel::<DynamicResult>(2);
        // Batch 1: Empty only, channel still open.
        tx.try_send(DynamicResult::Empty {
            provider: ProviderTag::Script("npm".into()),
        })
        .unwrap();
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.try_merge_dynamic(&parser, &mut buf);

        assert_eq!(handler.pending_empty_count, 1);
        assert!(handler.feedback.is_loading());

        // Batch 2: Loaded then drop tx.
        tx.try_send(DynamicResult::Loaded {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
            suggestions: vec![Suggestion {
                text: "main".into(),
                kind: SuggestionKind::GitBranch,
                source: SuggestionSource::Git,
                ..Default::default()
            }],
        })
        .unwrap();
        drop(tx);
        handler.try_merge_dynamic(&parser, &mut buf);

        assert_eq!(handler.pending_empty_count, 0, "drained on disconnect");
        assert_eq!(handler.pending_failed.len(), 0, "no errors accumulated");
    }

    // --- try_merge_dynamic disconnect branches ---

    #[test]
    fn test_try_merge_dynamic_error_only_disconnect_yields_error() {
        let mut handler = make_visible_handler(Vec::new());
        let base_ctx = gc_buffer::parse_command_context("", 0);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));

        let (tx, rx) = mpsc::channel::<DynamicResult>(1);
        tx.try_send(DynamicResult::Error {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
            message: "boom".into(),
        })
        .unwrap();
        drop(tx);
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.try_merge_dynamic(&parser, &mut buf);

        match handler.feedback_kind() {
            AsyncFeedback::Error { failed, .. } => {
                assert_eq!(failed.len(), 1, "single failed provider expected");
            }
            other => panic!("expected Error feedback, got {other:?}"),
        }
    }

    #[test]
    fn test_try_merge_dynamic_partial_error_with_static_present() {
        // Pre-seed a static suggestion, then deliver one Loaded + one Error
        // with a final disconnect. PartialError must result.
        let mut handler = make_visible_handler(vec![Suggestion {
            text: "--flag".into(),
            kind: SuggestionKind::Flag,
            source: SuggestionSource::Spec,
            ..Default::default()
        }]);
        let base_ctx = gc_buffer::parse_command_context("", 0);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));

        let (tx, rx) = mpsc::channel::<DynamicResult>(2);
        tx.try_send(DynamicResult::Loaded {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
            suggestions: vec![Suggestion {
                text: "main".into(),
                kind: SuggestionKind::GitBranch,
                source: SuggestionSource::Git,
                ..Default::default()
            }],
        })
        .unwrap();
        tx.try_send(DynamicResult::Error {
            provider: ProviderTag::Script("npm".into()),
            message: "oops".into(),
        })
        .unwrap();
        drop(tx);
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.try_merge_dynamic(&parser, &mut buf);

        match handler.feedback_kind() {
            AsyncFeedback::PartialError { failed, .. } => {
                assert_eq!(failed.len(), 1);
            }
            other => panic!("expected PartialError feedback, got {other:?}"),
        }
    }

    #[test]
    fn test_try_merge_dynamic_empty_only_with_no_static_yields_empty() {
        let mut handler = make_visible_handler(Vec::new());
        let base_ctx = gc_buffer::parse_command_context("", 0);
        handler.dynamic_ctx = Some(DynamicCtxSnapshot::capture(&base_ctx, false));

        let (tx, rx) = mpsc::channel::<DynamicResult>(1);
        tx.try_send(DynamicResult::Empty {
            provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
        })
        .unwrap();
        drop(tx);
        handler.dynamic_rx = Some(rx);

        let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
        let mut buf = Vec::new();
        handler.try_merge_dynamic(&parser, &mut buf);

        assert!(matches!(
            handler.feedback_kind(),
            AsyncFeedback::Empty { .. }
        ));
    }

    // --- current_feedback_kind redaction and Error → PartialError fallthrough ---

    #[test]
    fn test_current_feedback_kind_redacts_provider_when_disabled() {
        let mut handler = make_handler();
        handler.theme.show_provider_errors = false;
        handler.feedback = AsyncFeedback::Error {
            failed: vec!["git script".into()],
            since: std::time::Instant::now(),
        };
        match handler.current_feedback_kind() {
            gc_overlay::FeedbackKind::Error { provider } => {
                assert_eq!(provider, "", "provider must be redacted");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn test_current_feedback_kind_surfaces_provider_when_enabled() {
        let mut handler = make_handler();
        handler.theme.show_provider_errors = true;
        handler.feedback = AsyncFeedback::Error {
            failed: vec!["git script".into()],
            since: std::time::Instant::now(),
        };
        match handler.current_feedback_kind() {
            gc_overlay::FeedbackKind::Error { provider } => {
                assert_eq!(provider, "git script");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn test_current_feedback_kind_multi_failed_falls_through_to_partial_error() {
        let mut handler = make_handler();
        handler.feedback = AsyncFeedback::Error {
            failed: vec!["a".into(), "b".into(), "c".into()],
            since: std::time::Instant::now(),
        };
        match handler.current_feedback_kind() {
            gc_overlay::FeedbackKind::PartialError { providers } => {
                assert_eq!(providers, 3);
            }
            other => panic!("expected PartialError, got {other:?}"),
        }
    }

    #[test]
    fn test_current_feedback_kind_partial_error_clamps_at_u8_max() {
        let mut handler = make_handler();
        handler.feedback = AsyncFeedback::PartialError {
            failed: (0..300).map(|i| format!("p{i}")).collect(),
            since: std::time::Instant::now(),
        };
        match handler.current_feedback_kind() {
            gc_overlay::FeedbackKind::PartialError { providers } => {
                assert_eq!(providers, u8::MAX);
            }
            other => panic!("expected PartialError, got {other:?}"),
        }
    }
}
