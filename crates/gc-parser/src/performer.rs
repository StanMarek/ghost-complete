use std::path::PathBuf;

use unicode_width::UnicodeWidthChar;
use vte::Perform;

use crate::state::{CprOwner, TerminalState};

/// Helper: extract the first value from a CSI param subslice, or return the given default.
fn csi_param(params: &vte::Params, index: usize, default: u16) -> u16 {
    params
        .iter()
        .nth(index)
        .and_then(|sub| sub.first().copied())
        .map(|v| if v == 0 { default } else { v })
        .unwrap_or(default)
}

impl Perform for TerminalState {
    fn print(&mut self, c: char) {
        let width = UnicodeWidthChar::width(c).unwrap_or(1) as u16;
        self.advance_col(width);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0A => self.line_feed(),
            0x0D => self.carriage_return(),
            0x08 => self.backspace(),
            0x09 => self.tab(),
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        // vte sets `ignore = true` when the sequence could not be parsed
        // cleanly — e.g., parameter list overflowed `MAX_PARAMS` (currently
        // 32) or too many intermediates arrived. Applying a truncated
        // sequence would mean acting on garbage coordinates (CUP with a
        // dropped row/col drifts the cursor), so bail before touching state.
        if ignore {
            return;
        }
        // Blanket-discard any CSI sequence carrying intermediate bytes.
        // Examples: `CSI ? 25 h` (DECSET show cursor, intermediate `?`),
        // `CSI ! p` (DECSTR soft reset, intermediate `!`), `CSI > c` (DA2).
        // None of these affect the subset of state we track (cursor
        // position, screen dimensions, prompt/cwd bookkeeping), so the
        // cleanest handling is to ignore them entirely. This is a
        // deliberate narrowing of the state machine, not an oversight —
        // if a future feature ever needs to honor a specific
        // `CSI <intermediate> <final>` sequence, it MUST pattern-match on
        // the intermediate BEFORE this early return runs, not after.
        if !intermediates.is_empty() {
            return;
        }

        match action {
            // CUP — cursor position
            'H' | 'f' => {
                let row = csi_param(params, 0, 1).saturating_sub(1);
                let col = csi_param(params, 1, 1).saturating_sub(1);
                self.set_cursor(row, col);
            }
            // CUU — cursor up
            'A' => self.move_up(csi_param(params, 0, 1)),
            // CUD — cursor down
            'B' => self.move_down(csi_param(params, 0, 1)),
            // CUF — cursor forward
            'C' => self.move_forward(csi_param(params, 0, 1)),
            // CUB — cursor back
            'D' => self.move_back(csi_param(params, 0, 1)),
            // CNL — cursor next line
            'E' => {
                self.move_down(csi_param(params, 0, 1));
                self.carriage_return();
            }
            // CPL — cursor previous line
            'F' => {
                self.move_up(csi_param(params, 0, 1));
                self.carriage_return();
            }
            // CHA — cursor horizontal absolute
            'G' => {
                let col = csi_param(params, 0, 1).saturating_sub(1);
                self.set_col(col);
            }
            // VPA — vertical position absolute
            'd' => {
                let row = csi_param(params, 0, 1).saturating_sub(1);
                self.set_row(row);
            }
            // ED — erase in display
            'J' => {
                let mode = csi_param(params, 0, 0);
                if mode == 2 || mode == 3 {
                    self.set_cursor(0, 0);
                }
            }
            // SU — scroll up: content scrolls, cursor does NOT move
            'S' => {}
            // SD — scroll down: content scrolls, cursor does NOT move
            'T' => {}
            // ANSI save/restore cursor (SCO sequences)
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            // DSR — Device Status Report. Param 6 is "report cursor
            // position" (CSI 6n); the terminal will reply with
            // `CSI row;col R`. Enqueue with `CprOwner::Shell` so Task A
            // forwards the response back to the program inside the PTY
            // that asked for it. Other DSR variants (e.g. CSI 5n) do not
            // produce a CPR response and must NOT enqueue. The local
            // intermediates guard is defensive: the blanket-discard
            // above already drops sequences with intermediates, but
            // scoping the check here keeps DEC private DSR (`CSI ? 6n`)
            // safe even if the outer filter is ever relocated.
            'n' if intermediates.is_empty() && csi_param(params, 0, 0) == 6 => {
                self.enqueue_cpr(CprOwner::Shell);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        // See `csi_dispatch`: honor vte's `ignore` flag so we never act on
        // a truncated/malformed ESC sequence.
        if ignore {
            return;
        }
        // DECSC/DECRC can come as ESC 7 / ESC 8 (no intermediates)
        // or as CSI ? s / CSI ? u (which we don't handle here)
        if intermediates.is_empty() {
            match byte {
                b'7' => self.save_cursor(),
                b'8' => self.restore_cursor(),
                b'M' => self.reverse_index(),
                _ => {}
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }

        match params[0] {
            // OSC 133 — semantic prompts (FinalTerm protocol)
            b"133" => {
                if params.len() < 2 {
                    return;
                }
                match params[1] {
                    b"A" => {
                        // Prompt about to be displayed — request CPR sync so
                        // the proxy can query the real terminal for the actual
                        // cursor position and correct any VT tracking drift.
                        self.request_cursor_sync();
                        self.set_prompt_row(self.cursor_row());
                        self.set_in_prompt(true);
                        tracing::debug!(
                            row = self.cursor_row(),
                            "OSC 133;A — prompt start, CPR sync requested"
                        );
                    }
                    b"B" => {
                        // Prompt ended, command input starts
                        // (Some shells emit B; we treat it like A's complement)
                    }
                    b"C" => {
                        // Command execution started
                        self.set_in_prompt(false);
                        self.clear_command_buffer();
                        tracing::debug!("OSC 133;C — command executing");
                    }
                    _ if params[1].starts_with(b"D") => {
                        // Command finished (optional exit status follows)
                        // Future use — we don't need this yet
                    }
                    _ => {}
                }
            }
            // OSC 7771 — Ghost Complete prompt boundary (terminal-agnostic)
            // Mirrors OSC 133 behavior but is emitted by our shell integration
            // on all terminals, including those without native OSC 133 support.
            b"7771" => {
                if params.len() < 2 {
                    tracing::debug!("OSC 7771 with no subcommand — ignoring");
                    return;
                }
                match params[1] {
                    b"A" => {
                        self.request_cursor_sync();
                        self.set_prompt_row(self.cursor_row());
                        self.set_in_prompt(true);
                        tracing::debug!(
                            row = self.cursor_row(),
                            "OSC 7771;A — prompt start (shell integration)"
                        );
                    }
                    b"C" => {
                        self.set_in_prompt(false);
                        self.clear_command_buffer();
                        tracing::debug!("OSC 7771;C — command executing (shell integration)");
                    }
                    other => {
                        tracing::trace!(
                            sub = ?String::from_utf8_lossy(other),
                            "OSC 7771 unknown subcommand"
                        );
                    }
                }
            }
            // OSC 7772 — Ghost Complete buffer report (percent-encoded, secure framing).
            //
            // See `docs/adr/0003-osc7772-buffer-framing.md`. Replaces the
            // raw 7770 framing whose `;`/`\a`/`\e` bytes silently
            // truncated buffers and could smuggle nested OSC sequences.
            //
            //   params[0] = "7772"
            //   params[1] = cursor position (decimal char count)
            //   params[2] = percent-encoded UTF-8 buffer
            //
            // Anything malformed (bad cursor int, bad escape, non-UTF-8
            // payload) drops the frame (logs a `tracing::warn!`; prior
            // buffer state untouched).
            b"7772" => {
                if params.len() < 3 {
                    tracing::debug!(
                        params_len = params.len(),
                        "OSC 7772 — missing payload param, dropping frame"
                    );
                    return;
                }
                let cursor = match std::str::from_utf8(params[1])
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    Some(c) => c,
                    None => {
                        tracing::warn!(
                            "OSC 7772 — invalid cursor position, skipping buffer update"
                        );
                        return;
                    }
                };
                let decoded = match percent_decode_buffer(params[2]) {
                    Some(bytes) => bytes,
                    None => {
                        tracing::warn!("OSC 7772 — malformed percent escape in payload, skipping");
                        return;
                    }
                };
                let buffer = match String::from_utf8(decoded) {
                    Ok(s) => s,
                    Err(_) => {
                        tracing::warn!(
                            "OSC 7772 — invalid UTF-8 after decode, skipping buffer update"
                        );
                        return;
                    }
                };
                tracing::debug!(cursor, "OSC 7772 — buffer update");
                self.set_command_buffer(buffer, cursor);
            }
            // OSC 7770 — Ghost Complete buffer report (LEGACY raw framing).
            //
            // DEPRECATED: this path is structurally unsafe. vte splits OSC
            // params on `;`, so a buffer like `if true; then` is silently
            // truncated at the first semicolon, and embedded `\a` / `\e]`
            // bytes can prematurely terminate the OSC envelope or smuggle
            // a nested OSC into the parser. New shell integrations emit
            // OSC 7772 (percent-encoded) instead. See ADR 0003.
            //
            // Decoding/state-update logic is unchanged; only logging now
            // flags the legacy framing (one-shot `warn!` on first hit per
            // parser instance, `trace!` thereafter). Slated for removal at
            // v0.12.0.
            b"7770" => {
                if self.check_and_set_legacy_osc7770_warned() {
                    tracing::warn!(
                        "OSC 7770 (legacy raw framing) received — upgrade your shell \
                         integration. See docs/adr/0003-osc7772-buffer-framing.md."
                    );
                } else {
                    tracing::trace!("OSC 7770 (legacy) — buffer update");
                }
                if params.len() < 3 {
                    tracing::debug!("OSC 7770 — short params, dropping");
                    return;
                }
                let cursor = match std::str::from_utf8(params[1])
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    Some(c) => c,
                    None => {
                        tracing::warn!(
                            "OSC 7770 — invalid cursor position, skipping buffer update"
                        );
                        return;
                    }
                };
                let buffer = match String::from_utf8(params[2].to_vec()) {
                    Ok(s) => s,
                    Err(_) => {
                        tracing::warn!("OSC 7770 — invalid UTF-8 in buffer report, skipping");
                        return;
                    }
                };
                self.set_command_buffer(buffer, cursor);
            }
            // OSC 7 — current working directory
            b"7" => {
                if params.len() < 2 {
                    return;
                }
                if let Some(path) = parse_osc7_path(params[1]) {
                    tracing::debug!(?path, "OSC 7 — cwd update");
                    self.set_cwd(path);
                } else {
                    tracing::debug!(
                        raw = %String::from_utf8_lossy(params[1]),
                        "OSC 7 — failed to parse cwd URI"
                    );
                }
            }
            _ => {}
        }
    }
}

/// Parse a `file://{host}/{path}` URI from OSC 7 into a `PathBuf`.
///
/// Returns `None` if the URI is malformed, the decoded path is not absolute,
/// or contains traversal components (`.` or `..`). Legitimate shells always
/// report fully-resolved absolute paths in OSC 7 — traversal components in
/// the decoded path indicate hostile input and are rejected outright.
fn parse_osc7_path(uri: &[u8]) -> Option<PathBuf> {
    let s = std::str::from_utf8(uri).ok()?;
    let path_part = s.strip_prefix("file://")?;
    // Skip the hostname — find the first '/' after the authority
    let slash_idx = path_part.find('/')?;
    let path = &path_part[slash_idx..];
    // Percent-decode the path (handles all percent-encoded bytes)
    let decoded = percent_decode_path(path);
    // Reject non-absolute paths and any path with traversal components
    validate_osc7_cwd(&decoded)
}

/// Validate an OSC 7 CWD path: must be absolute with no `.` or `..` components.
///
/// Rejects (returns `None`) rather than normalizes — resolving `..` would hand
/// the attacker the exact directory they targeted. Legitimate shells never emit
/// traversal components in CWD reports.
///
/// Note: `Path::components()` silently absorbs `.` on absolute paths (never
/// yields `CurDir`), so we also check `ParentDir` via components AND `.`/`..`
/// via raw `OsStr` path segments.
fn validate_osc7_cwd(path: &std::path::Path) -> Option<PathBuf> {
    use std::path::Component;

    if !path.is_absolute() {
        return None;
    }
    // components() catches ".." (yields ParentDir) but silently drops "."
    for comp in path.components() {
        if matches!(comp, Component::ParentDir) {
            return None;
        }
    }
    // Catch "." segments that components() silently normalized away
    use std::os::unix::ffi::OsStrExt;
    for segment in path.as_os_str().as_bytes().split(|&b| b == b'/') {
        if segment == b"." || segment == b".." {
            return None;
        }
    }
    Some(path.to_path_buf())
}

/// Minimal percent-decoding for file paths.
///
/// Returns a `PathBuf` built directly from raw bytes via `OsStr`, avoiding
/// lossy UTF-8 conversion. Unix paths are byte sequences, not necessarily
/// valid UTF-8, so this preserves paths that contain arbitrary bytes.
fn percent_decode_path(input: &str) -> PathBuf {
    let mut bytes = Vec::with_capacity(input.len());
    let mut iter = input.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            match (iter.next(), iter.next()) {
                (Some(hi), Some(lo)) => {
                    if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                        bytes.push(h << 4 | l);
                    } else {
                        // Invalid hex — keep all three bytes literal
                        bytes.push(b'%');
                        bytes.push(hi);
                        bytes.push(lo);
                    }
                }
                (Some(hi), None) => {
                    // Truncated — keep literal
                    bytes.push(b'%');
                    bytes.push(hi);
                }
                (None, _) => {
                    // % at very end
                    bytes.push(b'%');
                }
            }
        } else {
            bytes.push(b);
        }
    }
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))
}

/// Percent-decode an OSC 7772 buffer payload.
///
/// Returns `None` on:
///   - invalid hex digit in `%XX`
///   - truncated `%` at end of input
///
/// This is intentionally STRICTER than [`percent_decode_path`]: a buffer
/// payload should round-trip exactly, so any malformed escape is a
/// transport error and the whole frame is dropped. The OSC 7 path
/// (filesystem URIs) instead preserves malformed bytes literally because
/// "best-effort partial CWD" is a sensible degradation, but for command
/// buffers the only safe action is to ignore the report.
fn percent_decode_buffer(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut iter = input.iter().copied();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let hi = iter.next()?;
            let lo = iter.next()?;
            let h = hex_val(hi)?;
            let l = hex_val(lo)?;
            out.push((h << 4) | l);
        } else {
            out.push(b);
        }
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TerminalParser;

    fn make_parser() -> TerminalParser {
        TerminalParser::new(24, 80)
    }

    // -- Ignore flag honoring --

    #[test]
    fn test_csi_ignore_flag_honored_on_param_overflow() {
        // vte sets `ignore = true` on a CSI dispatch when the parameter
        // list overflows (current MAX_PARAMS = 32 in vte 0.15). The
        // performer MUST NOT mutate state when ignore is true, otherwise
        // truncated-param sequences get applied with bogus coordinates.
        let mut p = make_parser();
        // Establish a known cursor position first.
        p.process_bytes(b"\x1b[5;10H");
        assert_eq!(p.state().cursor_position(), (4, 9));

        // Feed a CSI CUP sequence with 200 `1;` params — well above any
        // reasonable vte MAX_PARAMS. vte will set ignore=true on dispatch.
        // Without honoring the ignore flag, the performer would read
        // params[0] and params[1] and move the cursor to (0, 0).
        let mut seq: Vec<u8> = b"\x1b[".to_vec();
        for _ in 0..200 {
            seq.extend_from_slice(b"1;");
        }
        seq.push(b'H');
        p.process_bytes(&seq);

        assert_eq!(
            p.state().cursor_position(),
            (4, 9),
            "CSI dispatch with ignore=true must not mutate state"
        );
    }

    // -- Basic cursor tracking --

    #[test]
    fn test_print_advances_cursor() {
        let mut p = make_parser();
        p.process_bytes(b"abc");
        assert_eq!(p.state().cursor_position(), (0, 3));
    }

    #[test]
    fn test_line_wrap() {
        let mut p = TerminalParser::new(24, 5);
        p.process_bytes(b"abcde");
        // After 5 chars in a 5-col terminal, should wrap to next line
        assert_eq!(p.state().cursor_position(), (1, 0));
    }

    #[test]
    fn test_cr_resets_col() {
        let mut p = make_parser();
        p.process_bytes(b"hello\r");
        assert_eq!(p.state().cursor_position(), (0, 0));
    }

    #[test]
    fn test_lf_advances_row() {
        let mut p = make_parser();
        p.process_bytes(b"hello\n");
        assert_eq!(p.state().cursor_position(), (1, 5));
    }

    #[test]
    fn test_backspace() {
        let mut p = make_parser();
        p.process_bytes(b"abc\x08");
        assert_eq!(p.state().cursor_position(), (0, 2));
    }

    #[test]
    fn test_backspace_saturates() {
        let mut p = make_parser();
        p.process_bytes(b"\x08\x08\x08");
        assert_eq!(p.state().cursor_position(), (0, 0));
    }

    #[test]
    fn test_tab_stop() {
        let mut p = make_parser();
        p.process_bytes(b"ab\t");
        // col 2, next tab stop at 8
        assert_eq!(p.state().cursor_position(), (0, 8));
    }

    #[test]
    fn test_tab_from_zero() {
        let mut p = make_parser();
        p.process_bytes(b"\t");
        assert_eq!(p.state().cursor_position(), (0, 8));
    }

    #[test]
    fn test_print_cjk_advances_two_cols() {
        let mut p = make_parser();
        // CJK character '日' (U+65E5) is fullwidth — occupies 2 terminal columns
        p.process_bytes("日".as_bytes());
        assert_eq!(p.state().cursor_position(), (0, 2));
    }

    #[test]
    fn test_print_mixed_ascii_cjk() {
        let mut p = make_parser();
        // "a日b" = 1 + 2 + 1 = 4 columns
        p.process_bytes("a日b".as_bytes());
        assert_eq!(p.state().cursor_position(), (0, 4));
    }

    #[test]
    fn test_print_cjk_wraps_correctly() {
        let mut p = TerminalParser::new(24, 5);
        // 3 CJK chars (2 cols each) in a 5-col terminal:
        // '日' at col 0 → occupies cols 0-1, cursor at col 2
        // '本' at col 2 → occupies cols 2-3, cursor at col 4
        // '語' at col 4 → needs 2 cols but only 1 left, wraps first,
        //   then occupies row 1 cols 0-1, cursor at col 2
        p.process_bytes("日本語".as_bytes());
        assert_eq!(p.state().cursor_position(), (1, 2));
    }

    #[test]
    fn test_print_cjk_exact_fit_no_early_wrap() {
        let mut p = TerminalParser::new(24, 4);
        // 2 CJK chars (2 cols each) in 4-col terminal — exact fit
        // '日' cols 0-1, '本' cols 2-3, cursor wraps to (1, 0)
        p.process_bytes("日本".as_bytes());
        assert_eq!(p.state().cursor_position(), (1, 0));
    }

    #[test]
    fn test_print_cjk_wrap_in_3_col_terminal() {
        let mut p = TerminalParser::new(24, 3);
        // '日' at col 0 → cols 0-1, cursor at col 2
        // '本' at col 2 → needs 2 cols, only 1 left, wrap first
        //   → row 1 cols 0-1, cursor at col 2
        p.process_bytes("日本".as_bytes());
        assert_eq!(p.state().cursor_position(), (1, 2));
    }

    // -- CSI cursor movement --

    #[test]
    fn test_csi_cup() {
        let mut p = make_parser();
        // ESC[5;10H — cursor to row 5, col 10 (1-indexed)
        p.process_bytes(b"\x1b[5;10H");
        assert_eq!(p.state().cursor_position(), (4, 9));
    }

    #[test]
    fn test_csi_cup_defaults() {
        let mut p = make_parser();
        p.process_bytes(b"hello"); // move cursor
        p.process_bytes(b"\x1b[H"); // CUP with no params → (1,1) → (0,0)
        assert_eq!(p.state().cursor_position(), (0, 0));
    }

    #[test]
    fn test_csi_cursor_up() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[10;1H"); // go to row 10
        p.process_bytes(b"\x1b[3A"); // up 3
        assert_eq!(p.state().cursor_position(), (6, 0));
    }

    #[test]
    fn test_csi_cursor_down() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[2B"); // down 2
        assert_eq!(p.state().cursor_position(), (2, 0));
    }

    #[test]
    fn test_csi_cursor_forward() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[5C"); // forward 5
        assert_eq!(p.state().cursor_position(), (0, 5));
    }

    #[test]
    fn test_csi_cursor_back() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[20G"); // col 20 (1-indexed)
        p.process_bytes(b"\x1b[3D"); // back 3
        assert_eq!(p.state().cursor_position(), (0, 16));
    }

    #[test]
    fn test_csi_cha() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[15G"); // CHA to col 15 (1-indexed)
        assert_eq!(p.state().cursor_position(), (0, 14));
    }

    #[test]
    fn test_csi_vpa() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[10d"); // VPA to row 10 (1-indexed)
        assert_eq!(p.state().cursor_position(), (9, 0));
    }

    #[test]
    fn test_csi_cnl() {
        let mut p = make_parser();
        p.process_bytes(b"hello"); // col 5
        p.process_bytes(b"\x1b[2E"); // CNL: down 2, col 0
        assert_eq!(p.state().cursor_position(), (2, 0));
    }

    #[test]
    fn test_csi_cpl() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[10;15H"); // row 10, col 15
        p.process_bytes(b"\x1b[3F"); // CPL: up 3, col 0
        assert_eq!(p.state().cursor_position(), (6, 0));
    }

    #[test]
    fn test_csi_ed_clear_screen() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[10;15H"); // move cursor
        p.process_bytes(b"\x1b[2J"); // ED mode 2: clear screen
        assert_eq!(p.state().cursor_position(), (0, 0));
    }

    // -- Cursor save/restore --

    #[test]
    fn test_decsc_decrc() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[5;10H"); // move to (4, 9)
        p.process_bytes(b"\x1b7"); // DECSC: save
        p.process_bytes(b"\x1b[1;1H"); // move to (0, 0)
        assert_eq!(p.state().cursor_position(), (0, 0));
        p.process_bytes(b"\x1b8"); // DECRC: restore
        assert_eq!(p.state().cursor_position(), (4, 9));
    }

    #[test]
    fn test_reverse_index() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[5;1H"); // row 5
        p.process_bytes(b"\x1bM"); // RI: up 1
        assert_eq!(p.state().cursor_position(), (3, 0));
    }

    // -- OSC sequences --

    #[test]
    fn test_osc133_prompt_a() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[3;1H"); // row 3
        p.process_bytes(b"\x1b]133;A\x07"); // OSC 133;A (BEL terminated)
        assert_eq!(p.state().prompt_row(), Some(2));
        assert!(p.state().in_prompt());
        assert!(p.state_mut().take_cursor_sync_requested());
    }

    #[test]
    fn test_osc133_prompt_c() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]133;A\x07"); // start prompt
        assert!(p.state().in_prompt());
        p.process_bytes(b"\x1b]133;C\x07"); // command executing
        assert!(!p.state().in_prompt());
    }

    // -- OSC 7771 prompt boundary (terminal-agnostic) --

    #[test]
    fn test_osc7771_prompt_a() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[3;1H"); // row 3
        p.process_bytes(b"\x1b]7771;A\x07");
        assert_eq!(p.state().prompt_row(), Some(2));
        assert!(p.state().in_prompt());
        assert!(p.state_mut().take_cursor_sync_requested());
    }

    #[test]
    fn test_osc7771_prompt_c() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7771;A\x07"); // start prompt
        assert!(p.state().in_prompt());
        p.process_bytes(b"\x1b]7771;C\x07"); // command executing
        assert!(!p.state().in_prompt());
    }

    #[test]
    fn test_osc7771_clears_buffer_on_c() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7770;3;git\x07");
        assert_eq!(p.state().command_buffer(), Some("git"));
        p.process_bytes(b"\x1b]7771;C\x07");
        assert_eq!(p.state().command_buffer(), None);
        assert_eq!(p.state().buffer_cursor(), 0);
    }

    #[test]
    fn test_osc7771_short_params_no_crash() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[3;1H"); // set known cursor position
        p.process_bytes(b"\x1b]7771\x07");
        assert!(!p.state().in_prompt());
        assert_eq!(p.state().prompt_row(), None);
    }

    #[test]
    fn test_osc7771_unknown_subcommand_no_state_change() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[3;1H");
        p.process_bytes(b"\x1b]7771;B\x07");
        assert!(!p.state().in_prompt());
        assert_eq!(p.state().prompt_row(), None);
    }

    // -- Cross-protocol interaction (OSC 133 + 7771) --

    #[test]
    fn test_osc7771_a_then_osc133_c() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7771;A\x07"); // start via 7771
        assert!(p.state().in_prompt());
        p.process_bytes(b"\x1b]133;C\x07"); // end via 133
        assert!(!p.state().in_prompt());
    }

    #[test]
    fn test_osc133_a_then_osc7771_c() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]133;A\x07"); // start via 133
        assert!(p.state().in_prompt());
        p.process_bytes(b"\x1b]7771;C\x07"); // end via 7771
        assert!(!p.state().in_prompt());
    }

    // -- OSC 7770 buffer reporting --

    #[test]
    fn test_osc7770_buffer() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7770;5;hello\x07");
        assert_eq!(p.state().command_buffer(), Some("hello"));
        assert_eq!(p.state().buffer_cursor(), 5);
    }

    #[test]
    fn test_osc7770_empty() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7770;0;\x07");
        assert_eq!(p.state().command_buffer(), Some(""));
        assert_eq!(p.state().buffer_cursor(), 0);
    }

    #[test]
    fn test_osc7770_with_spaces() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7770;4;echo hello world\x07");
        assert_eq!(p.state().command_buffer(), Some("echo hello world"));
        assert_eq!(p.state().buffer_cursor(), 4);
    }

    #[test]
    fn test_buffer_cleared_on_command_exec() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7770;3;git\x07");
        assert_eq!(p.state().command_buffer(), Some("git"));
        p.process_bytes(b"\x1b]133;C\x07");
        assert_eq!(p.state().command_buffer(), None);
        assert_eq!(p.state().buffer_cursor(), 0);
    }

    // -- OSC 7 CWD --

    #[test]
    fn test_osc7_cwd() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7;file://localhost/Users/test\x07");
        assert_eq!(p.state().cwd(), Some(&PathBuf::from("/Users/test")));
    }

    #[test]
    fn test_osc7_cwd_with_spaces() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7;file://localhost/Users/test%20dir/sub\x07");
        assert_eq!(p.state().cwd(), Some(&PathBuf::from("/Users/test dir/sub")));
    }

    // -- Screen resize --

    #[test]
    fn test_update_dimensions() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[24;80H"); // bottom-right corner (0-indexed: 23, 79)
        assert_eq!(p.state().cursor_position(), (23, 79));

        // Shrink screen — cursor should be clamped
        p.state_mut().update_dimensions(10, 40);
        assert_eq!(p.state().cursor_position(), (9, 39));
        assert_eq!(p.state().screen_dimensions(), (10, 40));
    }

    // -- Helper unit tests --

    #[test]
    fn test_parse_osc7_path() {
        assert_eq!(
            parse_osc7_path(b"file://hostname/some/path"),
            Some(PathBuf::from("/some/path"))
        );
    }

    #[test]
    fn test_parse_osc7_path_percent_encoding() {
        assert_eq!(
            parse_osc7_path(b"file://host/path%20with%20spaces"),
            Some(PathBuf::from("/path with spaces"))
        );
    }

    #[test]
    fn test_parse_osc7_path_invalid() {
        assert_eq!(parse_osc7_path(b"not-a-file-uri"), None);
    }

    #[test]
    fn test_parse_osc7_path_traversal_percent_encoded_rejected() {
        // %2e%2e decodes to ".." — must be REJECTED, not normalized
        assert_eq!(
            parse_osc7_path(b"file://host/home/user/%2e%2e/%2e%2e/etc/passwd"),
            None
        );
    }

    #[test]
    fn test_parse_osc7_path_traversal_past_root_rejected() {
        assert_eq!(
            parse_osc7_path(b"file://host/%2e%2e/%2e%2e/%2e%2e/%2e%2e"),
            None
        );
    }

    #[test]
    fn test_parse_osc7_path_traversal_literal_dotdot_rejected() {
        assert_eq!(parse_osc7_path(b"file://host/a/b/../c"), None);
    }

    #[test]
    fn test_parse_osc7_path_dot_segment_rejected() {
        assert_eq!(parse_osc7_path(b"file://host/a/./b"), None);
    }

    #[test]
    fn test_validate_osc7_cwd_absolute() {
        assert_eq!(
            validate_osc7_cwd(std::path::Path::new("/a/b/c")),
            Some(PathBuf::from("/a/b/c"))
        );
    }

    #[test]
    fn test_validate_osc7_cwd_relative_rejected() {
        assert_eq!(
            validate_osc7_cwd(std::path::Path::new("relative/path")),
            None
        );
        assert_eq!(validate_osc7_cwd(std::path::Path::new("../sneaky")), None);
        assert_eq!(validate_osc7_cwd(std::path::Path::new("")), None);
    }

    #[test]
    fn test_validate_osc7_cwd_traversal_rejected() {
        assert_eq!(validate_osc7_cwd(std::path::Path::new("/a/./b/../c")), None);
        assert_eq!(
            validate_osc7_cwd(std::path::Path::new("/a/b/c/../../d")),
            None
        );
    }

    #[test]
    fn test_osc7770_sets_buffer_dirty() {
        let mut p = make_parser();
        assert!(!p.state_mut().take_buffer_dirty());
        p.process_bytes(b"\x1b]7770;3;git\x07");
        assert!(p.state_mut().take_buffer_dirty());
    }

    #[test]
    fn test_take_buffer_dirty_clears_flag() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7770;3;git\x07");
        assert!(p.state_mut().take_buffer_dirty());
        assert!(!p.state_mut().take_buffer_dirty());
    }

    // -- OSC 7 cwd_dirty flag --

    #[test]
    fn test_osc7_sets_cwd_dirty() {
        let mut p = make_parser();
        assert!(!p.state_mut().take_cwd_dirty());
        p.process_bytes(b"\x1b]7;file://localhost/Users/test\x07");
        assert!(p.state_mut().take_cwd_dirty());
    }

    #[test]
    fn test_take_cwd_dirty_clears_flag() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7;file://localhost/Users/test\x07");
        assert!(p.state_mut().take_cwd_dirty());
        assert!(!p.state_mut().take_cwd_dirty());
    }

    #[test]
    fn test_osc7_same_path_not_dirty() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7;file://localhost/Users/test\x07");
        assert!(p.state_mut().take_cwd_dirty());
        // Same path again — should NOT set dirty
        p.process_bytes(b"\x1b]7;file://localhost/Users/test\x07");
        assert!(!p.state_mut().take_cwd_dirty());
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(
            percent_decode_path("/hello%20world"),
            PathBuf::from("/hello world")
        );
        assert_eq!(
            percent_decode_path("/no/encoding"),
            PathBuf::from("/no/encoding")
        );
        assert_eq!(percent_decode_path("%2F"), PathBuf::from("/"));
    }

    #[test]
    fn test_percent_decode_basic() {
        assert_eq!(percent_decode_path("/foo%20bar"), PathBuf::from("/foo bar"));
        assert_eq!(
            percent_decode_path("/no/encoding"),
            PathBuf::from("/no/encoding")
        );
        assert_eq!(percent_decode_path(""), PathBuf::from(""));
    }

    #[test]
    fn test_percent_decode_preserves_malformed() {
        // Invalid hex digits — keep all bytes literal
        assert_eq!(
            percent_decode_path("/foo%zz/bar"),
            PathBuf::from("/foo%zz/bar")
        );
        assert_eq!(percent_decode_path("/foo%gh"), PathBuf::from("/foo%gh"));
    }

    #[test]
    fn test_percent_decode_truncated() {
        // % at end of string
        assert_eq!(percent_decode_path("/foo%"), PathBuf::from("/foo%"));
        // % with only one byte after
        assert_eq!(percent_decode_path("/foo%a"), PathBuf::from("/foo%a"));
    }

    #[test]
    fn test_percent_decode_utf8() {
        // é = U+00E9 = UTF-8 bytes C3 A9
        assert_eq!(percent_decode_path("/caf%C3%A9"), PathBuf::from("/café"));
        // 日 = U+65E5 = UTF-8 bytes E6 97 A5
        assert_eq!(percent_decode_path("/%E6%97%A5"), PathBuf::from("/日"));
    }

    #[test]
    fn test_percent_decode_literal_percent() {
        // %25 is the encoding for literal %
        assert_eq!(
            percent_decode_path("/100%25done"),
            PathBuf::from("/100%done")
        );
    }

    // -- percent_decode_buffer (strict: rejects malformed escapes) --

    #[test]
    fn percent_decode_buffer_empty() {
        assert_eq!(percent_decode_buffer(b""), Some(Vec::new()));
    }

    #[test]
    fn percent_decode_buffer_no_encoding() {
        assert_eq!(percent_decode_buffer(b"abc xyz"), Some(b"abc xyz".to_vec()));
    }

    #[test]
    fn percent_decode_buffer_single_escape() {
        assert_eq!(percent_decode_buffer(b"%20"), Some(b" ".to_vec()));
    }

    #[test]
    fn percent_decode_buffer_mixed() {
        // `if true; then` with `;` encoded.
        assert_eq!(
            percent_decode_buffer(b"if true%3B then"),
            Some(b"if true; then".to_vec())
        );
    }

    #[test]
    fn percent_decode_buffer_invalid_hex_rejected() {
        assert_eq!(percent_decode_buffer(b"ab%zz"), None);
        assert_eq!(percent_decode_buffer(b"ab%2g"), None);
    }

    #[test]
    fn percent_decode_buffer_truncated_rejected() {
        assert_eq!(percent_decode_buffer(b"ab%"), None);
        assert_eq!(percent_decode_buffer(b"ab%2"), None);
    }

    // -- OSC 7770 buffer cursor clamping --

    #[test]
    fn test_osc7770_cursor_clamped_to_buffer_length() {
        let mut p = make_parser();
        // cursor=9999 for a 3-char buffer — must clamp to 3
        p.process_bytes(b"\x1b]7770;9999;abc\x07");
        assert_eq!(p.state().command_buffer(), Some("abc"));
        assert_eq!(p.state().buffer_cursor(), 3);
    }

    #[test]
    fn test_osc7770_cursor_exact_length_not_clamped() {
        let mut p = make_parser();
        // cursor == buffer length is valid (cursor at end)
        p.process_bytes(b"\x1b]7770;5;hello\x07");
        assert_eq!(p.state().buffer_cursor(), 5);
    }

    #[test]
    fn test_predict_command_buffer_clamps_cursor() {
        let mut p = make_parser();
        p.state_mut()
            .predict_command_buffer("ls -la".to_string(), 9999);
        assert_eq!(p.state().buffer_cursor(), 6);
    }

    #[test]
    fn test_percent_decode_non_utf8_bytes() {
        // Bytes 0x80 0x81 are not valid UTF-8, but are valid Unix path bytes
        use std::os::unix::ffi::OsStrExt;
        let result = percent_decode_path("/%80%81");
        let expected = PathBuf::from(std::ffi::OsStr::from_bytes(&[b'/', 0x80, 0x81]));
        assert_eq!(result, expected);
    }

    // -- OSC 7770 UTF-8 rejection --

    // LEGACY: this test pins the OSC 7770 truncation bug deliberately —
    // the parser still accepts the 7770 framing for one deprecation cycle
    // even though it is structurally broken. Slated for `#[ignore]` at
    // v0.11.0 and deletion at v0.12.0, once stale shells have been pushed
    // to the OSC 7772 framing. See ADR 0003 and `osc7772_regression_pin_*`
    // below for the positive case the new framing fixes.
    //
    // vte splits OSC parameters on `;`, so a buffer like `if true; then`
    // becomes a 4-param OSC: `params[2] = "if true"`, `params[3] = " then"`.
    // The 7770 dispatch arm only reads `params[2]`, silently truncating
    // the buffer at the first semicolon.
    #[test]
    fn osc7770_legacy_truncates_on_semicolon_documented() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7770;14;if true; then\x07");
        assert_eq!(
            p.state().command_buffer(),
            Some("if true"),
            "legacy OSC 7770 truncates at first ';' — see ADR 0003"
        );
        // Cursor was 14 (one past the end of the full 13-char buffer);
        // clamped to the truncated buffer length 7.
        assert_eq!(p.state().buffer_cursor(), 7);
    }

    #[test]
    fn test_osc7770_invalid_utf8_rejected() {
        let mut p = make_parser();
        // Send OSC 7770 with invalid UTF-8 bytes (0xFF 0xFE) in the buffer payload.
        // Must be silently rejected — no buffer update, no replacement chars.
        let mut seq = Vec::new();
        seq.extend_from_slice(b"\x1b]7770;3;");
        seq.extend_from_slice(&[0xFF, 0xFE, 0x80]); // invalid UTF-8
        seq.push(0x07); // BEL terminator
        p.process_bytes(&seq);
        assert_eq!(p.state().command_buffer(), None);
        assert_eq!(p.state().buffer_cursor(), 0);
    }

    #[test]
    fn test_osc7770_valid_utf8_accepted() {
        // Valid UTF-8 (including multi-byte: café) must still work.
        let mut seq = Vec::new();
        seq.extend_from_slice(b"\x1b]7770;4;");
        seq.extend_from_slice("café".as_bytes());
        seq.push(0x07);

        let mut p = make_parser();
        p.process_bytes(&seq);
        assert_eq!(p.state().command_buffer(), Some("café"));
    }

    // -- CPR auto-enqueue on CSI 6n --

    #[test]
    fn csi_6n_enqueues_shell() {
        let mut p = make_parser();
        assert_eq!(p.state().cpr_queue_len(), 0);
        p.process_bytes(b"\x1b[6n");
        assert_eq!(p.state().cpr_queue_len(), 1);
        assert_eq!(p.state_mut().claim_next_cpr(), Some(crate::CprOwner::Shell));
    }

    #[test]
    fn csi_5n_does_not_enqueue() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[5n");
        assert_eq!(p.state().cpr_queue_len(), 0);
    }

    #[test]
    fn multiple_csi_6n_each_enqueue() {
        let mut p = make_parser();
        p.process_bytes(b"\x1b[6n\x1b[6n");
        assert_eq!(p.state().cpr_queue_len(), 2);
        assert_eq!(p.state_mut().claim_next_cpr(), Some(crate::CprOwner::Shell));
        assert_eq!(p.state_mut().claim_next_cpr(), Some(crate::CprOwner::Shell));
    }

    #[test]
    fn csi_private_6n_does_not_enqueue() {
        // CSI ? 6n is DEC private DSR — carries an intermediate byte `?`.
        // The blanket-discard at the top of csi_dispatch drops it before
        // the 'n' arm fires, so no Shell entry should be enqueued.
        let mut p = make_parser();
        p.process_bytes(b"\x1b[?6n");
        assert_eq!(p.state().cpr_queue_len(), 0);
    }

    // -- OSC 7772 buffer reporting (secure framing) --
    //
    // These tests own the canonical encoding contract. `encode_for_test`
    // is the spec the production zsh emitter MUST match byte-for-byte.
    // The decoder under test (`percent_decode_buffer`) MUST invert it
    // exactly. Any divergence between encoder allow-list and decoder
    // semantics shows up here first.

    /// Encode a byte slice for OSC 7772 transport. The allow-list mirrors
    /// `_gc_urlencode_buffer` in `shell/ghost-complete.zsh`:
    ///   unreserved bytes:  `[A-Za-z0-9._~/-]` and ` ` (literal space)
    ///   everything else  → `%XX` (uppercase hex)
    /// In particular `;`, `\x07`, `\x1B`, `%`, `\\`, `\x00`, control bytes,
    /// `0x7F`, and `0x80`–`0xFF` are all encoded.
    fn encode_for_test(input: &[u8]) -> Vec<u8> {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
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

    /// Wrap an already-encoded payload in the OSC 7772 envelope.
    fn build_osc7772(buffer: &[u8], cursor_chars: usize) -> Vec<u8> {
        let mut env = Vec::with_capacity(buffer.len() * 3 + 16);
        env.extend_from_slice(b"\x1b]7772;");
        env.extend_from_slice(cursor_chars.to_string().as_bytes());
        env.push(b';');
        env.extend_from_slice(&encode_for_test(buffer));
        env.push(0x07);
        env
    }

    fn assert_roundtrips(buffer: &str) {
        let mut p = make_parser();
        let cursor = buffer.chars().count();
        p.process_bytes(&build_osc7772(buffer.as_bytes(), cursor));
        assert_eq!(p.state().command_buffer(), Some(buffer));
        assert_eq!(p.state().buffer_cursor(), cursor);
    }

    #[test]
    fn osc7772_roundtrips_semicolon() {
        assert_roundtrips("if true; then");
    }

    // The canonical bug witness from the SPEC. Pre-OSC-7772 framing
    // truncated this buffer at the first `;`, surfacing as wrong
    // completion candidates the moment a user typed any composite
    // statement. Asserting whole-buffer reconstruction here keeps that
    // regression visible — if this ever fails, the encoder/decoder
    // contract has drifted and `cargo test` is the loud failure.
    #[test]
    fn osc7772_regression_pin_canonical_bug_witness() {
        assert_roundtrips("if true; then echo a; fi");
    }

    #[test]
    fn osc7772_roundtrips_bel() {
        assert_roundtrips("x\x07y");
    }

    #[test]
    fn osc7772_roundtrips_esc() {
        assert_roundtrips("\x1b[31m");
    }

    #[test]
    fn osc7772_roundtrips_nul() {
        assert_roundtrips("a\x00b");
    }

    #[test]
    fn osc7772_roundtrips_embedded_st() {
        assert_roundtrips("foo\x1b\\bar");
    }

    #[test]
    fn osc7772_roundtrips_long_8k() {
        // Deterministic 8 KiB ASCII pattern that exercises every byte the
        // emitter must encode (`;`, `%`, `\\`, control bytes) plus the
        // allowed unreserved alphabet. `cycle()` keeps the test offline
        // and reproducible without pulling a PRNG dependency.
        let pattern: &[u8] = b"a;b\\c%d e/f.g_h~i-j0 1\x07 \x1b2.3_4-5/6~7 8 9";
        let buf: Vec<u8> = pattern.iter().cycle().take(8192).copied().collect();
        let s = std::str::from_utf8(&buf).expect("ASCII fixture is valid UTF-8");
        assert_roundtrips(s);
    }

    #[test]
    fn osc7772_roundtrips_utf8_cjk() {
        assert_roundtrips("日本語");
    }

    #[test]
    fn osc7772_roundtrips_empty() {
        let mut p = make_parser();
        p.process_bytes(&build_osc7772(b"", 0));
        assert_eq!(p.state().command_buffer(), Some(""));
        assert_eq!(p.state().buffer_cursor(), 0);
    }

    #[test]
    fn osc7772_roundtrips_already_encoded_alphabet() {
        // The user typed the literal characters `abc%20def`. The encoder
        // must encode `%` as `%25` so the decoder yields back `abc%20def`,
        // not `abc def`. This pins "the decoder runs exactly once."
        assert_roundtrips("abc%20def");
    }

    #[test]
    fn osc7772_rejects_invalid_percent_escape() {
        let mut p = make_parser();
        // Establish a known-good prior buffer state.
        p.process_bytes(&build_osc7772(b"prior", 5));
        assert_eq!(p.state().command_buffer(), Some("prior"));
        // `%zz` has invalid hex digits — the whole frame must be rejected.
        p.process_bytes(b"\x1b]7772;3;ab%zz\x07");
        assert_eq!(
            p.state().command_buffer(),
            Some("prior"),
            "invalid percent escape must leave state untouched"
        );
        assert_eq!(p.state().buffer_cursor(), 5);
    }

    #[test]
    fn osc7772_rejects_truncated_percent() {
        let mut p = make_parser();
        p.process_bytes(&build_osc7772(b"prior", 5));
        p.process_bytes(b"\x1b]7772;2;ab%\x07");
        assert_eq!(p.state().command_buffer(), Some("prior"));
        assert_eq!(p.state().buffer_cursor(), 5);
    }

    #[test]
    fn osc7772_rejects_invalid_utf8_after_decode() {
        let mut p = make_parser();
        p.process_bytes(&build_osc7772(b"prior", 5));
        // Bytes 0xFF 0xFE 0x80 are decoded successfully but are not valid
        // UTF-8. Mirror the legacy 7770 path: silently drop, no replace
        // characters, no buffer mutation.
        p.process_bytes(b"\x1b]7772;3;%FF%FE%80\x07");
        assert_eq!(p.state().command_buffer(), Some("prior"));
        assert_eq!(p.state().buffer_cursor(), 5);
    }

    #[test]
    fn osc7772_security_no_nested_osc7_dispatch() {
        // Defense in depth: a buffer whose decoded value LOOKS like an
        // OSC 7 (`\e]7;file:///etc/passwd\a`) must NOT update CWD. The
        // decoded bytes go straight into `set_command_buffer`; they never
        // re-enter the VTE state machine.
        let mut p = make_parser();
        assert_eq!(p.state().cwd(), None);
        let smuggled = b"\x1b]7;file:///etc/passwd\x07";
        let cursor = std::str::from_utf8(smuggled).unwrap().chars().count();
        p.process_bytes(&build_osc7772(smuggled, cursor));
        assert_eq!(
            p.state().cwd(),
            None,
            "OSC 7772 payload must not re-enter the VTE state machine"
        );
        // The buffer itself reconstructs byte-for-byte.
        assert_eq!(
            p.state().command_buffer(),
            Some(std::str::from_utf8(smuggled).unwrap())
        );
    }

    #[test]
    fn osc7772_does_not_disturb_terminal_cursor() {
        let mut p = make_parser();
        p.process_bytes(b"hello");
        let before = p.state().cursor_position();
        p.process_bytes(&build_osc7772(b"if true; then", 13));
        p.process_bytes(b" world");
        assert_eq!(p.state().cursor_position(), (before.0, before.1 + 6));
        assert_eq!(p.state().command_buffer(), Some("if true; then"));
    }

    #[test]
    fn osc7772_rejects_non_numeric_cursor() {
        let mut p = make_parser();
        p.process_bytes(&build_osc7772(b"prior", 5));
        p.process_bytes(b"\x1b]7772;notanumber;abc\x07");
        assert_eq!(p.state().command_buffer(), Some("prior"));
        assert_eq!(p.state().buffer_cursor(), 5);
    }

    #[test]
    fn osc7772_rejects_negative_cursor() {
        let mut p = make_parser();
        p.process_bytes(&build_osc7772(b"prior", 5));
        p.process_bytes(b"\x1b]7772;-1;abc\x07");
        assert_eq!(p.state().command_buffer(), Some("prior"));
    }

    #[test]
    fn osc7772_rejects_missing_params() {
        let mut p = make_parser();
        p.process_bytes(&build_osc7772(b"prior", 5));
        p.process_bytes(b"\x1b]7772;5\x07");
        assert_eq!(p.state().command_buffer(), Some("prior"));
        p.process_bytes(b"\x1b]7772\x07");
        assert_eq!(p.state().command_buffer(), Some("prior"));
    }

    #[test]
    fn osc7772_cursor_clamped_to_buffer_length() {
        let mut p = make_parser();
        p.process_bytes(&build_osc7772(b"abc", 9999));
        assert_eq!(p.state().command_buffer(), Some("abc"));
        assert_eq!(p.state().buffer_cursor(), 3);
    }

    #[test]
    fn osc7772_cursor_zero_valid() {
        let mut p = make_parser();
        p.process_bytes(&build_osc7772(b"hello", 0));
        assert_eq!(p.state().buffer_cursor(), 0);
    }

    #[test]
    fn osc7770_legacy_dispatch_continues_after_warn_flag_flips() {
        // Three legacy frames in a row: the one-shot warn flag flips on
        // the first, but state updates must continue for every dispatch.
        let mut p = make_parser();
        p.process_bytes(b"\x1b]7770;1;a\x07");
        p.process_bytes(b"\x1b]7770;2;ab\x07");
        p.process_bytes(b"\x1b]7770;3;abc\x07");
        assert_eq!(p.state().command_buffer(), Some("abc"));
        assert_eq!(p.state().buffer_cursor(), 3);
    }
}
