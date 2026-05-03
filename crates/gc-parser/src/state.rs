use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Origin of a pending CPR (Cursor Position Report) request. Used by the
/// proxy to decide whether an incoming `CSI row;col R` response should be
/// consumed for ghost-complete's own cursor sync (`Ours`) or forwarded to
/// the program inside the PTY that asked for it (`Shell`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CprOwner {
    Ours,
    Shell,
}

/// Opaque handle returned by [`TerminalState::enqueue_cpr`]. Pass it to
/// [`TerminalState::rollback_cpr`] when a `CSI 6n` write fails partway so
/// the queued entry is removed without corrupting dispatch alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CprToken(u64);

#[derive(Debug)]
struct CprEntry {
    owner: CprOwner,
    id: u64,
    enqueued_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorSnapshot {
    row: u16,
    col: u16,
    pending_wrap: bool,
    autowrap: bool,
}

/// Tracks terminal state derived from the VT escape sequence stream.
///
/// Maintains cursor position, screen dimensions, prompt boundaries (OSC 133),
/// and current working directory (OSC 7). Updated by the `vte::Perform`
/// implementation in `performer.rs`.
#[derive(Debug)]
pub struct TerminalState {
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
    saved_cursor: Option<CursorSnapshot>,
    prompt_row: Option<u16>,
    autowrap: bool,
    pending_wrap: bool,
    display_dirty: bool,
    viewport_scroll_count: u16,
    cwd: Option<PathBuf>,
    in_prompt: bool,
    command_buffer: Option<String>,
    buffer_cursor: usize,
    buffer_dirty: bool,
    cwd_dirty: bool,
    cursor_sync_requested: bool,
    cpr_synced: bool,
    /// One-shot guard so the deprecation warning for the legacy OSC 7770
    /// raw-framing path fires at most once per `TerminalState` instance.
    /// Subsequent legacy dispatches downgrade to a `trace!` line so a stale
    /// shell does not spam the proxy log on every keystroke. Production
    /// currently constructs a single parser per proxy session, so this is
    /// effectively per-process. See ADR 0003.
    legacy_osc7770_warned: bool,
    /// FIFO queue of pending CPR requests in send-order.
    ///
    /// Terminals respond to `CSI 6n` requests in the same order they
    /// receive them. The queue head is therefore the owner of the next
    /// `CSI row;col R` response that will arrive on stdin. Task B pushes
    /// when it sends or observes a request; Task A pops the head when a
    /// response arrives. See `gc-pty/src/proxy.rs` for the call sites.
    cpr_queue: VecDeque<CprEntry>,
    /// Monotonic counter assigning unique IDs to CPR queue entries so
    /// `rollback_cpr` can locate and remove an entry even after Task A has
    /// popped earlier siblings.
    next_cpr_id: u64,
}

impl TerminalState {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            cursor_row: 0,
            cursor_col: 0,
            screen_rows: rows.max(1),
            screen_cols: cols.max(1),
            saved_cursor: None,
            prompt_row: None,
            autowrap: true,
            pending_wrap: false,
            display_dirty: false,
            viewport_scroll_count: 0,
            cwd: None,
            in_prompt: false,
            command_buffer: None,
            buffer_cursor: 0,
            buffer_dirty: false,
            cwd_dirty: false,
            cursor_sync_requested: false,
            cpr_synced: false,
            legacy_osc7770_warned: false,
            cpr_queue: VecDeque::new(),
            next_cpr_id: 0,
        }
    }

    /// Returns true the first time it is called per `TerminalState`,
    /// false thereafter. Used by the OSC 7770 (legacy) dispatch to log a
    /// one-shot deprecation warning while downgrading repeated hits to
    /// `trace!` to avoid spamming the log when a stale shell is talking
    /// to a new binary. Idempotent after first call. See ADR 0003.
    pub(crate) fn check_and_set_legacy_osc7770_warned(&mut self) -> bool {
        if self.legacy_osc7770_warned {
            false
        } else {
            self.legacy_osc7770_warned = true;
            true
        }
    }

    pub fn update_dimensions(&mut self, rows: u16, cols: u16) {
        self.screen_rows = rows.max(1);
        self.screen_cols = cols.max(1);
        self.pending_wrap = false;
        self.clamp_cursor();
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        (self.cursor_row, self.cursor_col)
    }

    pub fn screen_dimensions(&self) -> (u16, u16) {
        (self.screen_rows, self.screen_cols)
    }

    pub fn prompt_row(&self) -> Option<u16> {
        self.prompt_row
    }

    pub fn viewport_scroll_count(&self) -> u16 {
        self.viewport_scroll_count
    }

    pub fn cwd(&self) -> Option<&PathBuf> {
        self.cwd.as_ref()
    }

    pub fn in_prompt(&self) -> bool {
        self.in_prompt
    }

    pub fn command_buffer(&self) -> Option<&str> {
        self.command_buffer.as_deref()
    }

    pub fn buffer_cursor(&self) -> usize {
        self.buffer_cursor
    }

    /// Returns true if the command buffer was updated since the last check,
    /// and clears the flag atomically.
    pub fn take_buffer_dirty(&mut self) -> bool {
        let dirty = self.buffer_dirty;
        self.buffer_dirty = false;
        dirty
    }

    /// Returns true if the CWD changed since the last check,
    /// and clears the flag atomically.
    pub fn take_cwd_dirty(&mut self) -> bool {
        let dirty = self.cwd_dirty;
        self.cwd_dirty = false;
        dirty
    }

    pub fn take_display_dirty(&mut self) -> bool {
        let dirty = self.display_dirty;
        self.display_dirty = false;
        dirty
    }

    pub fn take_viewport_scroll_count(&mut self) -> u16 {
        let count = self.viewport_scroll_count;
        self.viewport_scroll_count = 0;
        count
    }

    /// Returns true if a CPR (Cursor Position Report) sync was requested
    /// since the last check, and clears the flag atomically.
    pub fn take_cursor_sync_requested(&mut self) -> bool {
        let requested = self.cursor_sync_requested;
        self.cursor_sync_requested = false;
        requested
    }

    /// Request a CPR-based cursor sync on the next opportunity.
    pub(crate) fn request_cursor_sync(&mut self) {
        self.cursor_sync_requested = true;
    }

    /// Validate that CPR coordinates (1-indexed) fall within screen bounds.
    /// Returns `false` for zero or out-of-range values, which indicate an
    /// injected or corrupted CPR response that should be discarded.
    pub fn validate_cpr_coordinates(&self, row_1: u16, col_1: u16) -> bool {
        row_1 > 0 && col_1 > 0 && row_1 <= self.screen_rows && col_1 <= self.screen_cols
    }

    /// Sync cursor position from a CPR response (1-indexed row/col from
    /// the terminal, converted to 0-indexed internally).
    pub fn set_cursor_from_report(&mut self, row_1: u16, col_1: u16) {
        self.cursor_row = row_1.saturating_sub(1);
        self.cursor_col = col_1.saturating_sub(1);
        self.pending_wrap = false;
        self.clamp_cursor();
        self.cpr_synced = true;
    }

    /// Returns true if a CPR sync completed since the last check,
    /// and clears the flag atomically. Used by the handler to know
    /// when the parser's cursor position has been corrected to match
    /// the real terminal, making any accumulated scroll deficit stale.
    pub fn take_cpr_synced(&mut self) -> bool {
        let synced = self.cpr_synced;
        self.cpr_synced = false;
        synced
    }

    /// Push a CPR request onto the back of the queue. Returns a token
    /// usable by [`Self::rollback_cpr`] if the corresponding `CSI 6n`
    /// write later fails.
    pub fn enqueue_cpr(&mut self, owner: CprOwner) -> CprToken {
        let id = self.next_cpr_id;
        self.next_cpr_id = self.next_cpr_id.wrapping_add(1);
        self.cpr_queue.push_back(CprEntry {
            owner,
            id,
            enqueued_at: Instant::now(),
        });
        CprToken(id)
    }

    /// Pop the oldest pending CPR entry. The owner identifies whether the
    /// matching response should be consumed locally (`Ours`) or forwarded
    /// to the PTY (`Shell`). Returns `None` if no request is outstanding —
    /// caller should log defensively and forward to the PTY in that case.
    pub fn claim_next_cpr(&mut self) -> Option<CprOwner> {
        self.cpr_queue.pop_front().map(|e| e.owner)
    }

    /// Remove the entry identified by `token` if it is still pending.
    /// Returns `true` if the entry was removed, `false` if it was already
    /// claimed by [`Self::claim_next_cpr`] before rollback could run
    /// (i.e., the response arrived after the write-failure was triggered
    /// but before Task B reached this code path).
    ///
    /// Used by Task B to undo a queued `Ours` entry when the corresponding
    /// `CSI 6n` write fails — without rollback, the orphan would shift
    /// dispatch alignment for every subsequent CPR until pruned.
    pub fn rollback_cpr(&mut self, token: CprToken) -> bool {
        // Queue depth is bounded 0–2 in practice (one Ours + one Shell
        // in flight at most). VecDeque::remove is O(n) on the slice,
        // but n is negligible here and rollback is off the hot path —
        // it fires only on stdout write/flush failure.
        if let Some(pos) = self.cpr_queue.iter().position(|e| e.id == token.0) {
            self.cpr_queue.remove(pos);
            true
        } else {
            false
        }
    }

    /// Drop CPR entries whose age exceeds `max_age`. A misbehaving terminal
    /// can fail to respond to a `CSI 6n`, leaving orphans in the queue
    /// forever. This is the leak guard — call once per Task B iteration
    /// with a generous timeout (e.g. 30s, well past z4h's 5s `read -srt 5`
    /// deadline). Returns the number of entries dropped so the caller can
    /// emit a `tracing::warn!`.
    pub fn prune_stale_cpr(&mut self, max_age: Duration) -> usize {
        let now = Instant::now();
        let before = self.cpr_queue.len();
        self.cpr_queue
            .retain(|e| now.duration_since(e.enqueued_at) < max_age);
        before - self.cpr_queue.len()
    }

    /// Number of outstanding CPR requests across both owners. Diagnostics
    /// and tests only — no dispatch logic should branch on this.
    pub fn cpr_queue_len(&self) -> usize {
        self.cpr_queue.len()
    }

    /// Override the command buffer with a predicted value (e.g., after Tab
    /// acceptance in directory chaining). Does NOT set `buffer_dirty` since
    /// this is a local prediction, not a shell-reported update via OSC 7770.
    pub fn predict_command_buffer(&mut self, buffer: String, cursor: usize) {
        self.buffer_cursor = cursor.min(buffer.chars().count());
        self.command_buffer = Some(buffer);
    }

    // -- mutation helpers used by Perform impl --

    pub(crate) fn set_cursor(&mut self, row: u16, col: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_row = row;
        self.cursor_col = col;
        self.clamp_cursor();
    }

    pub(crate) fn advance_col(&mut self, n: u16) {
        self.mark_display_dirty();
        if n == 0 {
            return;
        }

        if self.screen_cols == 0 {
            self.cursor_col = self.cursor_col.saturating_add(n);
            return;
        }

        if !self.autowrap {
            self.pending_wrap = false;
            self.cursor_col = self
                .cursor_col
                .saturating_add(n)
                .min(self.screen_cols.saturating_sub(1));
            return;
        }

        if self.pending_wrap {
            self.wrap_to_next_line();
        }
        self.pending_wrap = false;

        let next_col = self.cursor_col.saturating_add(n);
        if next_col < self.screen_cols {
            self.cursor_col = next_col;
        } else if next_col == self.screen_cols {
            self.cursor_col = self.screen_cols - 1;
            self.pending_wrap = true;
        } else {
            self.wrap_to_next_line();
            if n < self.screen_cols {
                self.cursor_col = n;
            } else {
                self.cursor_col = self.screen_cols - 1;
                self.pending_wrap = true;
            }
        }
    }

    pub(crate) fn move_up(&mut self, n: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_row = self.cursor_row.saturating_sub(n);
    }

    pub(crate) fn move_down(&mut self, n: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_row = self.cursor_row.saturating_add(n);
        self.clamp_cursor_row();
    }

    pub(crate) fn move_forward(&mut self, n: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_col = self.cursor_col.saturating_add(n);
        self.clamp_cursor_col();
    }

    pub(crate) fn move_back(&mut self, n: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_col = self.cursor_col.saturating_sub(n);
    }

    pub(crate) fn set_col(&mut self, col: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_col = col;
        self.clamp_cursor_col();
    }

    pub(crate) fn set_row(&mut self, row: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_row = row;
        self.clamp_cursor_row();
    }

    pub(crate) fn carriage_return(&mut self) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_col = 0;
    }

    pub(crate) fn line_feed(&mut self) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        if self.cursor_row + 1 >= self.screen_rows {
            self.record_viewport_scroll(1);
            self.cursor_row = self.screen_rows.saturating_sub(1);
        } else {
            self.cursor_row = self.cursor_row.saturating_add(1);
        }
    }

    pub(crate) fn backspace(&mut self) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    pub(crate) fn tab(&mut self) {
        self.mark_display_dirty();
        if self.pending_wrap {
            self.wrap_to_next_line();
            self.pending_wrap = false;
        }
        // Next tab stop: round up to next multiple of 8.
        // Saturating arithmetic prevents u16 overflow, and the max()
        // ensures monotonicity — tab never moves the cursor backward.
        let next = (self.cursor_col.saturating_add(8)) & !7;
        self.cursor_col = next.max(self.cursor_col);
        self.clamp_cursor_col();
    }

    pub(crate) fn save_cursor(&mut self) {
        self.saved_cursor = Some(CursorSnapshot {
            row: self.cursor_row,
            col: self.cursor_col,
            pending_wrap: self.pending_wrap,
            autowrap: self.autowrap,
        });
    }

    pub(crate) fn restore_cursor(&mut self) {
        self.mark_display_dirty();
        if let Some(snapshot) = self.saved_cursor {
            self.cursor_row = snapshot.row;
            self.cursor_col = snapshot.col;
            self.autowrap = snapshot.autowrap;
            self.pending_wrap = snapshot.pending_wrap;
            self.clamp_cursor();
        }
    }

    pub(crate) fn reverse_index(&mut self) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.cursor_row = self.cursor_row.saturating_sub(1);
    }

    pub(crate) fn set_prompt_row(&mut self, row: u16) {
        self.prompt_row = Some(row);
    }

    pub(crate) fn set_in_prompt(&mut self, in_prompt: bool) {
        self.in_prompt = in_prompt;
    }

    pub(crate) fn set_cwd(&mut self, path: PathBuf) {
        if self.cwd.as_ref() != Some(&path) {
            self.cwd = Some(path);
            self.cwd_dirty = true;
        }
    }

    pub(crate) fn set_command_buffer(&mut self, buffer: String, cursor: usize) {
        let clamped = cursor.min(buffer.chars().count());
        self.command_buffer = Some(buffer);
        self.buffer_cursor = clamped;
        self.buffer_dirty = true;
    }

    pub(crate) fn clear_command_buffer(&mut self) {
        self.command_buffer = None;
        self.buffer_cursor = 0;
    }

    pub(crate) fn set_autowrap(&mut self, enabled: bool) {
        self.autowrap = enabled;
        if !enabled {
            self.pending_wrap = false;
        }
    }

    pub(crate) fn scroll_up(&mut self, rows: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        self.record_viewport_scroll(rows);
    }

    pub(crate) fn scroll_down(&mut self, rows: u16) {
        self.mark_display_dirty();
        self.pending_wrap = false;
        if let Some(row) = self.prompt_row {
            let next = row.saturating_add(rows);
            self.prompt_row = (next < self.screen_rows).then_some(next);
        }
    }

    pub(crate) fn cursor_row(&self) -> u16 {
        self.cursor_row
    }

    fn clamp_cursor(&mut self) {
        self.clamp_cursor_row();
        self.clamp_cursor_col();
    }

    fn clamp_cursor_row(&mut self) {
        if self.screen_rows > 0 {
            self.cursor_row = self.cursor_row.min(self.screen_rows - 1);
        }
    }

    fn clamp_cursor_col(&mut self) {
        if self.screen_cols > 0 {
            self.cursor_col = self.cursor_col.min(self.screen_cols - 1);
        }
    }

    fn mark_display_dirty(&mut self) {
        self.display_dirty = true;
    }

    fn wrap_to_next_line(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row + 1 >= self.screen_rows {
            self.record_viewport_scroll(1);
            self.cursor_row = self.screen_rows.saturating_sub(1);
        } else {
            self.cursor_row = self.cursor_row.saturating_add(1);
        }
    }

    fn record_viewport_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.viewport_scroll_count = self.viewport_scroll_count.saturating_add(rows);
        if let Some(row) = self.prompt_row {
            self.prompt_row = row.checked_sub(rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_cpr_accepts_valid_coordinates() {
        let state = TerminalState::new(24, 80);
        assert!(state.validate_cpr_coordinates(1, 1));
        assert!(state.validate_cpr_coordinates(24, 80));
        assert!(state.validate_cpr_coordinates(12, 40));
    }

    #[test]
    fn validate_cpr_rejects_zero_coordinates() {
        let state = TerminalState::new(24, 80);
        assert!(!state.validate_cpr_coordinates(0, 1));
        assert!(!state.validate_cpr_coordinates(1, 0));
        assert!(!state.validate_cpr_coordinates(0, 0));
    }

    #[test]
    fn validate_cpr_rejects_out_of_bounds() {
        let state = TerminalState::new(24, 80);
        // Row beyond screen
        assert!(!state.validate_cpr_coordinates(25, 1));
        // Col beyond screen
        assert!(!state.validate_cpr_coordinates(1, 81));
        // Both beyond screen
        assert!(!state.validate_cpr_coordinates(25, 81));
        // Absurd injected values
        assert!(!state.validate_cpr_coordinates(65535, 65535));
    }

    #[test]
    fn validate_cpr_boundary_values() {
        let state = TerminalState::new(24, 80);
        // Exactly at screen bounds (valid — 1-indexed)
        assert!(state.validate_cpr_coordinates(24, 80));
        // One past screen bounds (invalid)
        assert!(!state.validate_cpr_coordinates(25, 80));
        assert!(!state.validate_cpr_coordinates(24, 81));
    }

    #[test]
    fn restore_cursor_clamps_after_resize() {
        let mut state = TerminalState::new(24, 80);
        // Save cursor near bottom-right of large terminal
        state.set_cursor(23, 79);
        state.save_cursor();
        // Shrink terminal
        state.update_dimensions(12, 40);
        // Restore — should clamp to new bounds
        state.restore_cursor();
        let (row, col) = state.cursor_position();
        assert!(row < 12, "row {row} should be clamped below 12");
        assert!(col < 40, "col {col} should be clamped below 40");
    }

    #[test]
    fn restore_cursor_restores_autowrap_and_pending_wrap() {
        let mut state = TerminalState::new(3, 3);
        state.set_cursor(0, 2);
        state.advance_col(1);
        assert_eq!(state.cursor_position(), (0, 2));
        assert!(state.pending_wrap);
        assert!(state.autowrap);

        state.save_cursor();
        state.set_autowrap(false);
        state.set_cursor(1, 0);

        state.restore_cursor();
        assert_eq!(state.cursor_position(), (0, 2));
        assert!(state.pending_wrap);
        assert!(state.autowrap);

        state.advance_col(1);
        assert_eq!(state.cursor_position(), (1, 1));
    }

    #[test]
    fn tab_saturating_at_u16_max() {
        let mut state = TerminalState::new(24, 80);
        let before = u16::MAX - 2;
        state.cursor_col = before;
        state.tab();
        // Should not panic, wrap, or go backward — cursor clamped to screen bounds
        let (_, col) = state.cursor_position();
        assert!(col < 80);
    }

    #[test]
    fn tab_never_moves_backward() {
        // Verify the raw tab-stop computation never goes backward.
        // We use a width of 65535 so clamping is (width-1) = 65534.
        // When cursor_col already exceeds the clamp boundary, the final
        // position will be clamped down — that's correct, not "backward".
        let width: u16 = 65535;
        for start in [65530u16, 65533, 65535, 65528] {
            let mut state = TerminalState::new(24, width);
            state.cursor_col = start;
            let before = start.min(width - 1); // effective position before tab
            state.tab();
            let (_, after) = state.cursor_position();
            assert!(
                after >= before,
                "tab moved cursor backward: {before} -> {after}"
            );
        }
    }

    #[test]
    fn zero_dimensions_clamped_to_one() {
        let state = TerminalState::new(0, 0);
        let (rows, cols) = state.screen_dimensions();
        assert_eq!(rows, 1);
        assert_eq!(cols, 1);
    }

    #[test]
    fn update_dimensions_clamps_zero() {
        let mut state = TerminalState::new(24, 80);
        state.update_dimensions(0, 0);
        let (rows, cols) = state.screen_dimensions();
        assert_eq!(rows, 1);
        assert_eq!(cols, 1);
    }

    #[test]
    fn line_feed_at_bottom_records_scroll_and_moves_prompt_row() {
        let mut state = TerminalState::new(3, 10);
        state.set_cursor(2, 0);
        state.set_prompt_row(1);

        state.line_feed();

        assert_eq!(state.cursor_position(), (2, 0));
        assert_eq!(state.prompt_row(), Some(0));
        assert_eq!(state.take_viewport_scroll_count(), 1);
        assert_eq!(state.take_viewport_scroll_count(), 0);
    }

    #[test]
    fn printing_last_column_defers_autowrap_until_next_printable() {
        let mut state = TerminalState::new(3, 3);

        state.advance_col(1);
        state.advance_col(1);
        state.advance_col(1);

        assert_eq!(state.cursor_position(), (0, 2));
        assert_eq!(state.take_viewport_scroll_count(), 0);

        state.advance_col(1);

        assert_eq!(state.cursor_position(), (1, 1));
    }

    #[test]
    fn pending_autowrap_at_bottom_records_scroll_on_next_printable() {
        let mut state = TerminalState::new(2, 3);
        state.set_cursor(1, 2);

        state.advance_col(1);
        assert_eq!(state.cursor_position(), (1, 2));
        assert_eq!(state.take_viewport_scroll_count(), 0);

        state.advance_col(1);

        assert_eq!(state.cursor_position(), (1, 1));
        assert_eq!(state.take_viewport_scroll_count(), 1);
    }

    #[test]
    fn cpr_queue_empty_by_default() {
        let state = TerminalState::new(24, 80);
        assert_eq!(state.cpr_queue_len(), 0);
    }

    #[test]
    fn enqueue_then_claim_returns_owner() {
        let mut state = TerminalState::new(24, 80);
        state.enqueue_cpr(CprOwner::Ours);
        assert_eq!(state.cpr_queue_len(), 1);
        assert_eq!(state.claim_next_cpr(), Some(CprOwner::Ours));
        assert_eq!(state.cpr_queue_len(), 0);
    }

    #[test]
    fn claim_returns_none_when_empty() {
        let mut state = TerminalState::new(24, 80);
        assert_eq!(state.claim_next_cpr(), None);
    }

    #[test]
    fn interleaved_enqueue_claims_in_fifo_order() {
        let mut state = TerminalState::new(24, 80);
        state.enqueue_cpr(CprOwner::Ours);
        state.enqueue_cpr(CprOwner::Shell);
        state.enqueue_cpr(CprOwner::Ours);
        assert_eq!(state.claim_next_cpr(), Some(CprOwner::Ours));
        assert_eq!(state.claim_next_cpr(), Some(CprOwner::Shell));
        assert_eq!(state.claim_next_cpr(), Some(CprOwner::Ours));
        assert_eq!(state.claim_next_cpr(), None);
    }

    #[test]
    fn enqueue_returns_unique_tokens() {
        let mut state = TerminalState::new(24, 80);
        let a = state.enqueue_cpr(CprOwner::Ours);
        let b = state.enqueue_cpr(CprOwner::Shell);
        assert_ne!(a, b);
    }

    #[test]
    fn rollback_removes_matching_token() {
        let mut state = TerminalState::new(24, 80);
        let token = state.enqueue_cpr(CprOwner::Ours);
        assert!(state.rollback_cpr(token));
        assert_eq!(state.cpr_queue_len(), 0);
    }

    #[test]
    fn rollback_returns_false_when_already_claimed() {
        let mut state = TerminalState::new(24, 80);
        let token = state.enqueue_cpr(CprOwner::Ours);
        let _ = state.claim_next_cpr();
        assert!(!state.rollback_cpr(token));
    }

    #[test]
    fn rollback_locates_entry_after_earlier_pops() {
        let mut state = TerminalState::new(24, 80);
        state.enqueue_cpr(CprOwner::Shell);
        let target = state.enqueue_cpr(CprOwner::Ours);
        state.enqueue_cpr(CprOwner::Shell);
        // Task A pops the first Shell entry.
        assert_eq!(state.claim_next_cpr(), Some(CprOwner::Shell));
        assert!(state.rollback_cpr(target));
        assert_eq!(state.cpr_queue_len(), 1);
        assert_eq!(state.claim_next_cpr(), Some(CprOwner::Shell));
    }

    #[test]
    fn prune_stale_drops_zero_when_all_fresh() {
        let mut state = TerminalState::new(24, 80);
        state.enqueue_cpr(CprOwner::Ours);
        state.enqueue_cpr(CprOwner::Shell);
        let dropped = state.prune_stale_cpr(Duration::from_secs(30));
        assert_eq!(dropped, 0);
        assert_eq!(state.cpr_queue_len(), 2);
    }

    #[test]
    fn prune_stale_drops_old_entries_only() {
        let mut state = TerminalState::new(24, 80);
        state.enqueue_cpr(CprOwner::Ours);
        // Ensure the first entry is measurably "old" before the second push.
        std::thread::sleep(Duration::from_millis(15));
        state.enqueue_cpr(CprOwner::Shell);
        // Prune anything older than 10ms — should drop only the first.
        let dropped = state.prune_stale_cpr(Duration::from_millis(10));
        assert_eq!(dropped, 1);
        assert_eq!(state.cpr_queue_len(), 1);
        assert_eq!(state.claim_next_cpr(), Some(CprOwner::Shell));
    }

    #[test]
    fn check_and_set_legacy_osc7770_warned_is_one_shot() {
        let mut s = TerminalState::new(24, 80);
        assert!(
            s.check_and_set_legacy_osc7770_warned(),
            "first call returns true"
        );
        assert!(
            !s.check_and_set_legacy_osc7770_warned(),
            "second call returns false"
        );
        assert!(
            !s.check_and_set_legacy_osc7770_warned(),
            "third call still false"
        );
        s.update_dimensions(48, 120);
        assert!(
            !s.check_and_set_legacy_osc7770_warned(),
            "resize must not reset"
        );
    }
}
