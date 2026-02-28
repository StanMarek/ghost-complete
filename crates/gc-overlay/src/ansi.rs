use std::io::Write;

/// Begin synchronized output (Ghostty DECSET 2026).
/// Terminal buffers all output until end_sync, then draws atomically.
pub fn begin_sync(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[?2026h");
}

/// End synchronized output.
pub fn end_sync(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[?2026l");
}

/// Save cursor position (DECSC).
pub fn save_cursor(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b7");
}

/// Restore cursor position (DECRC).
pub fn restore_cursor(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b8");
}

/// Move cursor to absolute position. Row/col are 0-indexed internally,
/// converted to 1-indexed for ANSI CUP sequence.
pub fn move_to(buf: &mut Vec<u8>, row: u16, col: u16) {
    let _ = write!(buf, "\x1b[{};{}H", row + 1, col + 1);
}

/// Set reverse video (for selected item highlight).
pub fn reverse_video(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[7m");
}

/// Set dim text (for descriptions).
pub fn dim(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[2m");
}

/// Set bold text.
pub fn bold(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[1m");
}

/// Reset all text attributes.
pub fn reset(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[0m");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_begin_sync() {
        let mut buf = Vec::new();
        begin_sync(&mut buf);
        assert_eq!(buf, b"\x1b[?2026h");
    }

    #[test]
    fn test_end_sync() {
        let mut buf = Vec::new();
        end_sync(&mut buf);
        assert_eq!(buf, b"\x1b[?2026l");
    }

    #[test]
    fn test_move_to_one_indexed() {
        let mut buf = Vec::new();
        move_to(&mut buf, 0, 0);
        assert_eq!(String::from_utf8_lossy(&buf), "\x1b[1;1H");
    }

    #[test]
    fn test_move_to_arbitrary() {
        let mut buf = Vec::new();
        move_to(&mut buf, 5, 10);
        assert_eq!(String::from_utf8_lossy(&buf), "\x1b[6;11H");
    }

    #[test]
    fn test_save_restore_cursor() {
        let mut buf = Vec::new();
        save_cursor(&mut buf);
        assert_eq!(buf, b"\x1b7");

        let mut buf2 = Vec::new();
        restore_cursor(&mut buf2);
        assert_eq!(buf2, b"\x1b8");
    }

    #[test]
    fn test_reverse_video() {
        let mut buf = Vec::new();
        reverse_video(&mut buf);
        assert_eq!(buf, b"\x1b[7m");
    }

    #[test]
    fn test_reset() {
        let mut buf = Vec::new();
        reset(&mut buf);
        assert_eq!(buf, b"\x1b[0m");
    }
}
