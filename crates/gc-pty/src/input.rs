/// Minimal key event parser for raw terminal stdin bytes.
///
/// Parses known sequences (arrows, Tab, Enter, Escape, Ctrl+Space, Ctrl+/,
/// Ctrl+A through Ctrl+Z) and passes through everything else as Raw bytes.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    Tab,
    Enter,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    PageUp,
    PageDown,
    Home,
    End,
    CtrlSpace,
    CtrlSlash,
    Ctrl(char),
    Backspace,
    Printable(char),
    /// Cursor Position Report response (CSI row;col R) — 1-indexed.
    CursorPositionReport(u16, u16),
    /// Unknown bytes — forward verbatim to PTY.
    Raw(Vec<u8>),
}

/// Parse a buffer of raw stdin bytes into key events.
///
/// One read() call can contain multiple keystrokes (e.g. fast typing or
/// paste). This returns them all in order.
pub fn parse_keys(buf: &[u8]) -> Vec<KeyEvent> {
    let mut events = Vec::new();
    let mut i = 0;

    while i < buf.len() {
        match buf[i] {
            0x00 => {
                events.push(KeyEvent::CtrlSpace);
                i += 1;
            }
            0x1F => {
                events.push(KeyEvent::CtrlSlash);
                i += 1;
            }
            0x09 => {
                events.push(KeyEvent::Tab);
                i += 1;
            }
            0x0D => {
                events.push(KeyEvent::Enter);
                i += 1;
            }
            0x7F => {
                events.push(KeyEvent::Backspace);
                i += 1;
            }
            0x1B => {
                // Escape or CSI sequence
                if i + 2 < buf.len() && buf[i + 1] == b'[' {
                    match buf[i + 2] {
                        b'A' => {
                            events.push(KeyEvent::ArrowUp);
                            i += 3;
                        }
                        b'B' => {
                            events.push(KeyEvent::ArrowDown);
                            i += 3;
                        }
                        b'C' => {
                            events.push(KeyEvent::ArrowRight);
                            i += 3;
                        }
                        b'D' => {
                            events.push(KeyEvent::ArrowLeft);
                            i += 3;
                        }
                        b'H' => {
                            events.push(KeyEvent::Home);
                            i += 3;
                        }
                        b'F' => {
                            events.push(KeyEvent::End);
                            i += 3;
                        }
                        b'1' if i + 3 < buf.len() && buf[i + 3] == b'~' => {
                            events.push(KeyEvent::Home);
                            i += 4;
                        }
                        b'4' if i + 3 < buf.len() && buf[i + 3] == b'~' => {
                            events.push(KeyEvent::End);
                            i += 4;
                        }
                        b'5' if i + 3 < buf.len() && buf[i + 3] == b'~' => {
                            events.push(KeyEvent::PageUp);
                            i += 4;
                        }
                        b'6' if i + 3 < buf.len() && buf[i + 3] == b'~' => {
                            events.push(KeyEvent::PageDown);
                            i += 4;
                        }
                        _ => {
                            // Unknown CSI — find end and pass through as Raw
                            let start = i;
                            i += 2; // skip ESC [
                                    // CSI params: bytes in 0x30-0x3F, intermediates: 0x20-0x2F
                                    // Final byte: 0x40-0x7E
                            while i < buf.len() && buf[i] < 0x40 {
                                i += 1;
                            }
                            if i < buf.len() {
                                let final_byte = buf[i];
                                i += 1; // consume final byte
                                        // Check for CPR response: CSI {row};{col} R
                                if final_byte == b'R' {
                                    if let Some((row, col)) = parse_cpr(&buf[start + 2..i - 1]) {
                                        events.push(KeyEvent::CursorPositionReport(row, col));
                                        continue;
                                    }
                                }
                            }
                            events.push(KeyEvent::Raw(buf[start..i].to_vec()));
                        }
                    }
                } else if i + 2 < buf.len() && buf[i + 1] == b'O' {
                    // SS3 sequences (some terminals use ESC O A for arrow keys)
                    match buf[i + 2] {
                        b'A' => {
                            events.push(KeyEvent::ArrowUp);
                            i += 3;
                        }
                        b'B' => {
                            events.push(KeyEvent::ArrowDown);
                            i += 3;
                        }
                        b'C' => {
                            events.push(KeyEvent::ArrowRight);
                            i += 3;
                        }
                        b'D' => {
                            events.push(KeyEvent::ArrowLeft);
                            i += 3;
                        }
                        b'H' => {
                            events.push(KeyEvent::Home);
                            i += 3;
                        }
                        b'F' => {
                            events.push(KeyEvent::End);
                            i += 3;
                        }
                        _ => {
                            events.push(KeyEvent::Raw(buf[i..i + 3].to_vec()));
                            i += 3;
                        }
                    }
                } else if i + 1 == buf.len() {
                    // Standalone ESC at end of buffer
                    events.push(KeyEvent::Escape);
                    i += 1;
                } else if i + 1 < buf.len() && buf[i + 1] != b'[' && buf[i + 1] != b'O' {
                    // ESC followed by something that's not [ or O (Alt+key)
                    events.push(KeyEvent::Raw(buf[i..i + 2].to_vec()));
                    i += 2;
                } else {
                    // ESC [ or ESC O but buffer too short — treat as raw
                    events.push(KeyEvent::Raw(buf[i..].to_vec()));
                    i = buf.len();
                }
            }
            b if (0x01..=0x08).contains(&b)
                || (0x0A..=0x0C).contains(&b)
                || (0x0E..=0x1A).contains(&b) =>
            {
                events.push(KeyEvent::Ctrl((b + 0x60) as char));
                i += 1;
            }
            b if (0x20..=0x7E).contains(&b) => {
                events.push(KeyEvent::Printable(b as char));
                i += 1;
            }
            _ => {
                // Control char or high byte — pass through
                events.push(KeyEvent::Raw(vec![buf[i]]));
                i += 1;
            }
        }
    }

    events
}

/// Parse a CPR parameter slice like b"15;1" into (row, col).
/// Returns None if the format doesn't match.
fn parse_cpr(params: &[u8]) -> Option<(u16, u16)> {
    let s = std::str::from_utf8(params).ok()?;
    let (row_s, col_s) = s.split_once(';')?;
    let row = row_s.parse::<u16>().ok()?;
    let col = col_s.parse::<u16>().ok()?;
    Some((row, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_printable_chars() {
        let events = parse_keys(b"abc");
        assert_eq!(
            events,
            vec![
                KeyEvent::Printable('a'),
                KeyEvent::Printable('b'),
                KeyEvent::Printable('c'),
            ]
        );
    }

    #[test]
    fn test_tab() {
        let events = parse_keys(b"\x09");
        assert_eq!(events, vec![KeyEvent::Tab]);
    }

    #[test]
    fn test_enter() {
        let events = parse_keys(b"\x0D");
        assert_eq!(events, vec![KeyEvent::Enter]);
    }

    #[test]
    fn test_backspace() {
        let events = parse_keys(b"\x7F");
        assert_eq!(events, vec![KeyEvent::Backspace]);
    }

    #[test]
    fn test_ctrl_space() {
        let events = parse_keys(b"\x00");
        assert_eq!(events, vec![KeyEvent::CtrlSpace]);
    }

    #[test]
    fn test_arrow_keys_csi() {
        assert_eq!(parse_keys(b"\x1B[A"), vec![KeyEvent::ArrowUp]);
        assert_eq!(parse_keys(b"\x1B[B"), vec![KeyEvent::ArrowDown]);
        assert_eq!(parse_keys(b"\x1B[C"), vec![KeyEvent::ArrowRight]);
        assert_eq!(parse_keys(b"\x1B[D"), vec![KeyEvent::ArrowLeft]);
    }

    #[test]
    fn test_page_up_csi_tilde() {
        assert_eq!(parse_keys(b"\x1B[5~"), vec![KeyEvent::PageUp]);
    }

    #[test]
    fn test_page_down_csi_tilde() {
        assert_eq!(parse_keys(b"\x1B[6~"), vec![KeyEvent::PageDown]);
    }

    #[test]
    fn test_home_csi_letter() {
        assert_eq!(parse_keys(b"\x1B[H"), vec![KeyEvent::Home]);
    }

    #[test]
    fn test_end_csi_letter() {
        assert_eq!(parse_keys(b"\x1B[F"), vec![KeyEvent::End]);
    }

    #[test]
    fn test_home_csi_tilde_synonym() {
        assert_eq!(parse_keys(b"\x1B[1~"), vec![KeyEvent::Home]);
    }

    #[test]
    fn test_end_csi_tilde_synonym() {
        assert_eq!(parse_keys(b"\x1B[4~"), vec![KeyEvent::End]);
    }

    #[test]
    fn test_arrow_keys_ss3() {
        assert_eq!(parse_keys(b"\x1BOA"), vec![KeyEvent::ArrowUp]);
        assert_eq!(parse_keys(b"\x1BOB"), vec![KeyEvent::ArrowDown]);
    }

    #[test]
    fn test_home_ss3() {
        assert_eq!(parse_keys(b"\x1BOH"), vec![KeyEvent::Home]);
    }

    #[test]
    fn test_end_ss3() {
        assert_eq!(parse_keys(b"\x1BOF"), vec![KeyEvent::End]);
    }

    #[test]
    fn test_standalone_escape() {
        let events = parse_keys(b"\x1B");
        assert_eq!(events, vec![KeyEvent::Escape]);
    }

    #[test]
    fn test_unknown_csi_passthrough() {
        // e.g. ESC [ 1 ; 5 C (Ctrl+Right in some terminals)
        let raw = b"\x1B[1;5C";
        let events = parse_keys(raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            KeyEvent::Raw(bytes) => assert_eq!(bytes, raw),
            other => panic!("expected Raw, got {:?}", other),
        }
    }

    #[test]
    fn test_paged_then_typed() {
        assert_eq!(
            parse_keys(b"\x1B[5~a"),
            vec![KeyEvent::PageUp, KeyEvent::Printable('a')]
        );
    }

    #[test]
    fn test_mixed_input() {
        // "a" then ArrowUp then "b"
        let events = parse_keys(b"a\x1B[Ab");
        assert_eq!(
            events,
            vec![
                KeyEvent::Printable('a'),
                KeyEvent::ArrowUp,
                KeyEvent::Printable('b'),
            ]
        );
    }

    #[test]
    fn test_empty_input() {
        let events = parse_keys(b"");
        assert!(events.is_empty());
    }

    #[test]
    fn test_cpr_response_parsed() {
        // CSI 15;1 R — cursor at row 15, col 1
        let events = parse_keys(b"\x1b[15;1R");
        assert_eq!(events, vec![KeyEvent::CursorPositionReport(15, 1)]);
    }

    #[test]
    fn test_cpr_response_large_values() {
        let events = parse_keys(b"\x1b[100;200R");
        assert_eq!(events, vec![KeyEvent::CursorPositionReport(100, 200)]);
    }

    #[test]
    fn test_cpr_mixed_with_typing() {
        // User types 'a', then CPR arrives, then user types 'b'
        let events = parse_keys(b"a\x1b[5;10Rb");
        assert_eq!(
            events,
            vec![
                KeyEvent::Printable('a'),
                KeyEvent::CursorPositionReport(5, 10),
                KeyEvent::Printable('b'),
            ]
        );
    }

    #[test]
    fn test_ctrl_slash() {
        let events = parse_keys(b"\x1F");
        assert_eq!(events, vec![KeyEvent::CtrlSlash]);
    }

    #[test]
    fn test_alt_key_passthrough() {
        // Alt+a = ESC a — should be Raw
        let events = parse_keys(b"\x1Ba");
        assert_eq!(events.len(), 1);
        match &events[0] {
            KeyEvent::Raw(bytes) => assert_eq!(bytes, b"\x1Ba"),
            other => panic!("expected Raw, got {:?}", other),
        }
    }

    #[test]
    fn test_ctrl_letters() {
        assert_eq!(parse_keys(b"\x01"), vec![KeyEvent::Ctrl('a')]);
        assert_eq!(parse_keys(b"\x04"), vec![KeyEvent::Ctrl('d')]);
        assert_eq!(parse_keys(b"\x03"), vec![KeyEvent::Ctrl('c')]);
        assert_eq!(parse_keys(b"\x1A"), vec![KeyEvent::Ctrl('z')]);
    }

    #[test]
    fn test_ctrl_mixed_with_printable() {
        let events = parse_keys(b"a\x04b");
        assert_eq!(
            events,
            vec![
                KeyEvent::Printable('a'),
                KeyEvent::Ctrl('d'),
                KeyEvent::Printable('b'),
            ]
        );
    }
}
