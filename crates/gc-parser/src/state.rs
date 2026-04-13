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
    saved_cursor: Option<(u16, u16)>,
    prompt_row: Option<u16>,
    cwd: Option<PathBuf>,
    in_prompt: bool,
    command_buffer: Option<String>,
    buffer_cursor: usize,
    buffer_dirty: bool,
    cwd_dirty: bool,
    cursor_sync_requested: bool,
    cpr_synced: bool,
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
            cwd: None,
            in_prompt: false,
            command_buffer: None,
            buffer_cursor: 0,
            buffer_dirty: false,
            cwd_dirty: false,
            cursor_sync_requested: false,
            cpr_synced: false,
            cpr_queue: VecDeque::new(),
            next_cpr_id: 0,
        }
    }

    pub fn update_dimensions(&mut self, rows: u16, cols: u16) {
        self.screen_rows = rows.max(1);
        self.screen_cols = cols.max(1);
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
        self.cursor_row = row;
        self.cursor_col = col;
        self.clamp_cursor();
    }

    pub(crate) fn advance_col(&mut self, n: u16) {
        if self.screen_cols > 0 {
            // Wide character (n > 1) doesn't fit in remaining columns —
            // real terminals wrap to the next line before placing it,
            // leaving the partial column blank.
            if n > 1 && self.cursor_col + n > self.screen_cols {
                self.cursor_row = self.cursor_row.saturating_add(1);
                self.cursor_col = if n < self.screen_cols { n } else { 0 };
            } else {
                self.cursor_col = self.cursor_col.saturating_add(n);
                if self.cursor_col >= self.screen_cols {
                    self.cursor_row = self
                        .cursor_row
                        .saturating_add(self.cursor_col / self.screen_cols);
                    self.cursor_col %= self.screen_cols;
                }
            }
            // Wrapping past the bottom row means the terminal scrolled.
            self.clamp_cursor_row();
        } else {
            self.cursor_col = self.cursor_col.saturating_add(n);
        }
    }

    pub(crate) fn move_up(&mut self, n: u16) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
    }

    pub(crate) fn move_down(&mut self, n: u16) {
        self.cursor_row = self.cursor_row.saturating_add(n);
        self.clamp_cursor_row();
    }

    pub(crate) fn move_forward(&mut self, n: u16) {
        self.cursor_col = self.cursor_col.saturating_add(n);
        self.clamp_cursor_col();
    }

    pub(crate) fn move_back(&mut self, n: u16) {
        self.cursor_col = self.cursor_col.saturating_sub(n);
    }

    pub(crate) fn set_col(&mut self, col: u16) {
        self.cursor_col = col;
        self.clamp_cursor_col();
    }

    pub(crate) fn set_row(&mut self, row: u16) {
        self.cursor_row = row;
        self.clamp_cursor_row();
    }

    pub(crate) fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    pub(crate) fn line_feed(&mut self) {
        self.cursor_row = self.cursor_row.saturating_add(1);
        // At the bottom of the screen, a real terminal scrolls rather than
        // moving the cursor past the last row.
        self.clamp_cursor_row();
    }

    pub(crate) fn backspace(&mut self) {
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    pub(crate) fn tab(&mut self) {
        // Next tab stop: round up to next multiple of 8.
        // Saturating arithmetic prevents u16 overflow, and the max()
        // ensures monotonicity — tab never moves the cursor backward.
        let next = (self.cursor_col.saturating_add(8)) & !7;
        self.cursor_col = next.max(self.cursor_col);
        self.clamp_cursor_col();
    }

    pub(crate) fn save_cursor(&mut self) {
        self.saved_cursor = Some((self.cursor_row, self.cursor_col));
    }

    pub(crate) fn restore_cursor(&mut self) {
        if let Some((row, col)) = self.saved_cursor {
            self.cursor_row = row;
            self.cursor_col = col;
            self.clamp_cursor();
        }
    }

    pub(crate) fn reverse_index(&mut self) {
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
}
