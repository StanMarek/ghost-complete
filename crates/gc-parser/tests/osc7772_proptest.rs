//! Property tests for the OSC 7772 framing.
//!
//! `build_osc7772` is the in-test port of `_gc_urlencode_buffer` in
//! `shell/ghost-complete.zsh`. The two implementations must match
//! byte-for-byte: every legal `$BUFFER` the shell can emit must round-trip
//! through this encoder and the production decoder. Any divergence here
//! is also a bug in `crates/gc-parser/src/performer.rs`.

use gc_parser::TerminalParser;
use proptest::prelude::*;

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Mirror `_gc_urlencode_buffer`: pass `[A-Za-z0-9._~/-]` and ` ` through;
/// percent-encode every other byte as `%XX`. UTF-8 multibyte sequences are
/// encoded byte-by-byte (each high byte matches `0x80..=0xFF` and is
/// outside the allow-list).
fn encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for &b in input {
        let safe = matches!(b,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'.' | b'_' | b'~' | b'/' | b'-' | b' '
        );
        if safe {
            out.push(b);
        } else {
            out.push(b'%');
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 0x0F) as usize]);
        }
    }
    out
}

fn build_osc7772(buffer: &[u8], cursor_chars: usize) -> Vec<u8> {
    let mut env = Vec::with_capacity(buffer.len() * 3 + 16);
    env.extend_from_slice(b"\x1b]7772;");
    env.extend_from_slice(cursor_chars.to_string().as_bytes());
    env.push(b';');
    env.extend_from_slice(&encode(buffer));
    env.push(0x07);
    env
}

proptest! {
    /// Any valid UTF-8 string up to 4 KiB must round-trip exactly.
    /// `proptest` regex `.{0,4096}` generates Unicode code points; the
    /// Rust `String` we get is by definition valid UTF-8.
    #[test]
    fn osc7772_roundtrips_arbitrary_utf8(s in ".{0,4096}") {
        let mut p = TerminalParser::new(24, 80);
        let env = build_osc7772(s.as_bytes(), s.chars().count());
        p.process_bytes(&env);
        prop_assert_eq!(p.state().command_buffer(), Some(s.as_str()));
    }

    /// Arbitrary byte vectors that are NOT valid UTF-8 must be silently
    /// dropped. The buffer state stays as it was prior to the malformed
    /// frame — no replacement chars, no partial decode.
    #[test]
    fn osc7772_rejects_invalid_utf8(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        prop_assume!(std::str::from_utf8(&bytes).is_err());
        let mut p = TerminalParser::new(24, 80);
        let before = p.state().command_buffer().map(str::to_string);
        let env = build_osc7772(&bytes, 0);
        p.process_bytes(&env);
        prop_assert_eq!(
            p.state().command_buffer().map(str::to_string),
            before
        );
    }
}
