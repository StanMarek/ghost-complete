use std::path::PathBuf;

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
}

impl TerminalState {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            cursor_row: 0,
            cursor_col: 0,
            screen_rows: rows,
            screen_cols: cols,
            saved_cursor: None,
            prompt_row: None,
            cwd: None,
            in_prompt: false,
            command_buffer: None,
            buffer_cursor: 0,
            buffer_dirty: false,
            cwd_dirty: false,
        }
    }

    pub fn update_dimensions(&mut self, rows: u16, cols: u16) {
        self.screen_rows = rows;
        self.screen_cols = cols;
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

    /// Override the command buffer with a predicted value (e.g., after Tab
    /// acceptance in directory chaining). Does NOT set `buffer_dirty` since
    /// this is a local prediction, not a shell-reported update via OSC 7770.
    pub fn predict_command_buffer(&mut self, buffer: String, cursor: usize) {
        self.command_buffer = Some(buffer);
        self.buffer_cursor = cursor;
    }

    // -- mutation helpers used by Perform impl --

    pub(crate) fn set_cursor(&mut self, row: u16, col: u16) {
        self.cursor_row = row;
        self.cursor_col = col;
        self.clamp_cursor();
    }

    pub(crate) fn advance_col(&mut self, n: u16) {
        self.cursor_col = self.cursor_col.saturating_add(n);
        if self.screen_cols > 0 && self.cursor_col >= self.screen_cols {
            self.cursor_row = self
                .cursor_row
                .saturating_add(self.cursor_col / self.screen_cols);
            self.cursor_col %= self.screen_cols;
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
    }

    pub(crate) fn backspace(&mut self) {
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    pub(crate) fn tab(&mut self) {
        self.cursor_col = (self.cursor_col + 8) & !7;
        self.clamp_cursor_col();
    }

    pub(crate) fn save_cursor(&mut self) {
        self.saved_cursor = Some((self.cursor_row, self.cursor_col));
    }

    pub(crate) fn restore_cursor(&mut self) {
        if let Some((row, col)) = self.saved_cursor {
            self.cursor_row = row;
            self.cursor_col = col;
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
        self.command_buffer = Some(buffer);
        self.buffer_cursor = cursor;
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
