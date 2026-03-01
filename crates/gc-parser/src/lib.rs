//! VT escape sequence parser for terminal state tracking.
//!
//! Uses the `vte` crate to parse ANSI/VT sequences and track cursor position,
//! screen dimensions, prompt boundaries (OSC 133), and CWD (OSC 7).

mod performer;
mod state;

pub use state::TerminalState;

/// Wraps `vte::Parser` and `TerminalState` into a single unit.
///
/// Feed terminal output bytes through [`process_bytes`](Self::process_bytes)
/// and query the resulting state via [`state`](Self::state).
pub struct TerminalParser {
    parser: vte::Parser,
    state: TerminalState,
}

impl TerminalParser {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vte::Parser::new(),
            state: TerminalState::new(rows, cols),
        }
    }

    /// Feed raw bytes from PTY output through the VT parser.
    pub fn process_bytes(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.state, bytes);
    }

    pub fn state(&self) -> &TerminalState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut TerminalState {
        &mut self.state
    }
}
