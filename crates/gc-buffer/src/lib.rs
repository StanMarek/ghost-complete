//! Command line buffer reconstruction and context detection.
//!
//! Tokenizes the raw command line and determines the current command, argument
//! position, pipe/redirect state, and quoting context for the suggestion engine.

mod context;
mod tokenizer;

pub use context::{parse_command_context, CommandContext};
pub use tokenizer::{tokenize, QuoteState, Token, TokenizeResult};

/// Convert a character offset to a byte offset within a UTF-8 string.
/// Returns `s.len()` if `char_offset` is beyond the end.
pub fn char_to_byte_offset(s: &str, char_offset: usize) -> usize {
    s.char_indices()
        .nth(char_offset)
        .map_or(s.len(), |(i, _)| i)
}

/// Convert a byte offset to a character offset within a UTF-8 string.
/// Clamps to `s.len()` if out of range, and snaps to the nearest char
/// boundary (searching backwards) if `byte_offset` falls mid-codepoint.
pub fn byte_to_char_offset(s: &str, byte_offset: usize) -> usize {
    let safe = byte_offset.min(s.len());
    let boundary = (0..=safe)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    s[..boundary].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_char_to_byte_ascii() {
        assert_eq!(char_to_byte_offset("hello", 3), 3);
    }

    #[test]
    fn test_char_to_byte_multibyte() {
        // "ąść" — each char is 2 bytes
        assert_eq!(char_to_byte_offset("ąść", 0), 0);
        assert_eq!(char_to_byte_offset("ąść", 1), 2);
        assert_eq!(char_to_byte_offset("ąść", 2), 4);
        assert_eq!(char_to_byte_offset("ąść", 3), 6);
    }

    #[test]
    fn test_char_to_byte_beyond_end() {
        assert_eq!(char_to_byte_offset("hi", 999), 2);
    }

    #[test]
    fn test_byte_to_char_multibyte() {
        assert_eq!(byte_to_char_offset("ąść", 0), 0);
        assert_eq!(byte_to_char_offset("ąść", 2), 1);
        assert_eq!(byte_to_char_offset("ąść", 4), 2);
        assert_eq!(byte_to_char_offset("ąść", 6), 3);
    }

    #[test]
    fn test_byte_to_char_beyond_end() {
        assert_eq!(byte_to_char_offset("hi", 999), 2);
    }

    #[test]
    fn test_byte_to_char_mid_codepoint() {
        // "ą" is 2 bytes (0xC4 0x85); offset 1 is mid-codepoint
        assert_eq!(byte_to_char_offset("ąść", 1), 0);
        assert_eq!(byte_to_char_offset("ąść", 3), 1);
        assert_eq!(byte_to_char_offset("ąść", 5), 2);
    }

    #[test]
    fn test_byte_to_char_empty_string() {
        assert_eq!(byte_to_char_offset("", 0), 0);
        assert_eq!(byte_to_char_offset("", 5), 0);
    }
}
