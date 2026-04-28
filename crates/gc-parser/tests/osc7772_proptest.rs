//! Property tests for the OSC 7772 framing.
//!
//! `encode` is the in-test port of `_gc_urlencode_buffer` in
//! `shell/ghost-complete.zsh`; `build_osc7772` wraps an encoded payload
//! in the OSC envelope. Both must match the production zsh emitter
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
    /// Any valid UTF-8 string of up to 4096 Unicode code points must
    /// round-trip exactly. `proptest` regex `.{0,4096}` generates Unicode
    /// scalar values; the resulting Rust `String` is by definition valid
    /// UTF-8.
    #[test]
    fn osc7772_roundtrips_arbitrary_utf8(s in ".{0,4096}") {
        let mut p = TerminalParser::new(24, 80);
        let env = build_osc7772(s.as_bytes(), s.chars().count());
        p.process_bytes(&env);
        prop_assert_eq!(p.state().command_buffer(), Some(s.as_str()));
    }

    /// Arbitrary byte vectors that are NOT valid UTF-8 must be silently
    /// dropped. The buffer state stays as it was prior to the malformed
    /// frame — no replacement chars, no partial decode. We seed a known
    /// prior `(buffer, cursor)` so the assertion fails if a rejection bug
    /// mutates either field independently.
    #[test]
    fn osc7772_rejects_invalid_utf8(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        prop_assume!(std::str::from_utf8(&bytes).is_err());
        let mut p = TerminalParser::new(24, 80);
        p.process_bytes(&build_osc7772(b"prior", 5));
        let env = build_osc7772(&bytes, 0);
        p.process_bytes(&env);
        prop_assert_eq!(p.state().command_buffer(), Some("prior"));
        prop_assert_eq!(p.state().buffer_cursor(), 5);
    }

    /// Malformed percent escapes (lone `%` at end, non-hex digit after
    /// `%`) must cause the entire frame to be rejected — buffer state
    /// stays as it was. The decoder is intentionally stricter than the
    /// OSC 7 path decoder: a buffer is uniformly percent-encoded by the
    /// shell, so any malformed escape is corruption, not legal content.
    ///
    /// The malformed token is appended after a well-formed prefix so the
    /// decoder sees a frame whose head looks legal but whose tail forces
    /// the rejection branches reached only by hand-picked unit tests in
    /// the encode-and-roundtrip strategies. Placing it at the end avoids
    /// "rescue by neighbor": e.g. a stray `%` followed by two hex digits
    /// in a randomly generated suffix would form a valid escape.
    #[test]
    fn osc7772_rejects_malformed_percent(
        prefix in "[A-Za-z0-9._~/ -]{0,64}",
        bad in r"(%|%[G-Zg-z]|%[0-9A-Fa-f][G-Zg-z]|%[G-Zg-z][0-9A-Fa-f])",
    ) {
        let mut p = TerminalParser::new(24, 80);
        p.process_bytes(&build_osc7772(b"prior", 5));

        let mut payload = Vec::new();
        payload.extend_from_slice(&encode(prefix.as_bytes()));
        payload.extend_from_slice(bad.as_bytes());

        let mut env = Vec::with_capacity(payload.len() + 16);
        env.extend_from_slice(b"\x1b]7772;0;");
        env.extend_from_slice(&payload);
        env.push(0x07);
        p.process_bytes(&env);

        prop_assert_eq!(p.state().command_buffer(), Some("prior"));
        prop_assert_eq!(p.state().buffer_cursor(), 5);
    }
}
