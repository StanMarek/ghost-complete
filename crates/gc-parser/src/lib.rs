//! VT escape sequence parser for terminal state tracking.
//!
//! Uses the `vte` crate to parse ANSI/VT sequences and track cursor position,
//! screen dimensions, prompt boundaries (OSC 133), and CWD (OSC 7).

mod performer;
mod state;

pub use state::{CprOwner, CprToken, Diagnostic, TerminalState};

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    /// Build an OSC 7772 envelope for testing: percent-encode the buffer
    /// with the same allow-list as `shell/ghost-complete.zsh`
    /// (`[A-Za-z0-9._~/-]` plus space).
    pub fn build_osc7772_envelope(buffer: &str, cursor: usize) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b]7772;");
        out.extend_from_slice(cursor.to_string().as_bytes());
        out.push(b';');
        for byte in buffer.bytes() {
            let allowed = matches!(byte,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
                | b'.' | b'_' | b'~' | b'/' | b'-' | b' '
            );
            if allowed {
                out.push(byte);
            } else {
                out.extend_from_slice(format!("%{:02X}", byte).as_bytes());
            }
        }
        out.push(0x07);
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_simple_buffer_space_passes_through() {
            // "git " with cursor=4: space is in the allow-list, passes literally
            let result = build_osc7772_envelope("git ", 4);
            assert_eq!(result, b"\x1b]7772;4;git \x07");
        }

        #[test]
        fn test_semicolon_is_percent_encoded() {
            // semicolons are OSC param delimiters — must be encoded
            let result = build_osc7772_envelope(";", 1);
            assert_eq!(result, b"\x1b]7772;1;%3B\x07");
        }

        #[test]
        fn test_esc_bel_percent_are_encoded() {
            // ESC (0x1B), BEL (0x07), and '%' (0x25) must all be percent-encoded
            let result = build_osc7772_envelope("\x1b", 1);
            assert_eq!(result, b"\x1b]7772;1;%1B\x07");

            let result2 = build_osc7772_envelope("\x07", 1);
            assert_eq!(result2, b"\x1b]7772;1;%07\x07");

            let result3 = build_osc7772_envelope("%", 1);
            assert_eq!(result3, b"\x1b]7772;1;%25\x07");
        }

        #[test]
        fn test_utf8_bytes_percent_encoded() {
            // UTF-8 multi-byte: "中" = 0xE4 0xB8 0xAD — each byte encoded
            let result = build_osc7772_envelope("中", 1);
            assert_eq!(result, b"\x1b]7772;1;%E4%B8%AD\x07");
        }

        /// Round-trip: feed the helper's output through `TerminalParser` and
        /// assert the decoded `(command_buffer, buffer_cursor)` matches the
        /// originals. Guards against an encoder regression (wrong allow-list
        /// char, missed percent-encoding for a control byte) silently
        /// producing envelopes the production decoder rejects — a class of
        /// bug otherwise visible only as opaque "popup did not render"
        /// PTY-smoke failures. Mirrors the parallel `build_osc7772` helper
        /// exercised by `tests/osc7772_proptest.rs`.
        #[test]
        fn roundtrip_ascii_only() {
            let buffer = "git status";
            let cursor = buffer.chars().count();
            let envelope = build_osc7772_envelope(buffer, cursor);

            let mut parser = crate::TerminalParser::new(24, 80);
            parser.process_bytes(&envelope);

            assert_eq!(parser.state().command_buffer(), Some(buffer));
            assert_eq!(parser.state().buffer_cursor(), cursor);
        }

        #[test]
        fn roundtrip_with_percent_encoded_semicolon() {
            // Semicolon is the OSC param delimiter — encoder must emit `%3B`
            // and the decoder must reverse it.
            let buffer = "echo a;b";
            let cursor = buffer.chars().count();
            let envelope = build_osc7772_envelope(buffer, cursor);

            let mut parser = crate::TerminalParser::new(24, 80);
            parser.process_bytes(&envelope);

            assert_eq!(parser.state().command_buffer(), Some(buffer));
            assert_eq!(parser.state().buffer_cursor(), cursor);
        }

        #[test]
        fn roundtrip_with_utf8_multibyte() {
            // "中" is three bytes (E4 B8 AD), all percent-encoded on the
            // wire, and counts as a single character for cursor positioning.
            let buffer = "echo 中";
            let cursor = buffer.chars().count();
            let envelope = build_osc7772_envelope(buffer, cursor);

            let mut parser = crate::TerminalParser::new(24, 80);
            parser.process_bytes(&envelope);

            assert_eq!(parser.state().command_buffer(), Some(buffer));
            assert_eq!(parser.state().buffer_cursor(), cursor);
        }
    }
}

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
