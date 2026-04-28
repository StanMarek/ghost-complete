//! End-to-end OSC 7772 round-trip: real zsh emitter → real Rust parser.
//!
//! Spawns an actual `zsh -c` for each fixture, sources the production
//! `shell/ghost-complete.zsh`, calls `_gc_report_buffer` with the fixture
//! bytes set as `$BUFFER`, captures stdout, and feeds the bytes through
//! `gc_parser::TerminalParser`. The reconstructed buffer must equal the
//! input. The OSC-injection fixture additionally asserts CWD did NOT
//! change — proving the decoded bytes never re-entered the VTE state
//! machine. See ADR 0003.
//!
//! Skipped silently on systems without `zsh` on PATH (e.g. Alpine CI).

use std::path::PathBuf;
use std::process::Command;

use gc_parser::TerminalParser;

/// Encode arbitrary bytes into a zsh `$'…'` literal. Every byte goes
/// through as `\xXX` — verbose but unambiguous, no quoting edge cases.
fn to_zsh_literal(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 + 3);
    out.push_str("$'");
    for &b in bytes {
        out.push_str(&format!("\\x{b:02X}"));
    }
    out.push('\'');
    out
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("gc-pty crate should be two dirs deep")
        .to_path_buf()
}

fn zsh_available() -> bool {
    Command::new("zsh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the production emitter for `(buffer, cursor)` and return raw stdout.
fn emit_via_real_zsh(buffer: &[u8], cursor: usize) -> Vec<u8> {
    let zsh_init = repo_root().join("shell/ghost-complete.zsh");
    assert!(zsh_init.exists(), "shell/ghost-complete.zsh not found");

    // Single-line zsh script: source the integration, set BUFFER and
    // CURSOR, invoke the reporter. `$'…'` escapes through every byte
    // literally — no quoting hazards.
    let script = format!(
        "source {init_q}; BUFFER={buf_lit}; CURSOR={cursor}; _gc_report_buffer",
        init_q = shell_quote(zsh_init.to_str().expect("path is utf-8")),
        buf_lit = to_zsh_literal(buffer),
    );

    let output = Command::new("zsh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("zsh -c failed to launch");
    assert!(
        output.status.success(),
        "zsh exited non-zero: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// POSIX-quote a string for safe interpolation as one zsh argument.
fn shell_quote(s: &str) -> String {
    // Single-quote everything; close-quote, escape any `'` as `'\''`,
    // reopen. Defensive even for paths we control — tempdirs are fine
    // but the repo path could in principle contain a quote.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn assert_roundtrips(label: &str, fixture: &[u8]) {
    let cursor = std::str::from_utf8(fixture)
        .expect("fixture is valid UTF-8")
        .chars()
        .count();
    let stdout = emit_via_real_zsh(fixture, cursor);

    let mut p = TerminalParser::new(24, 80);
    let cwd_before = p.state().cwd().cloned();
    p.process_bytes(&stdout);

    let actual = p.state().command_buffer();
    let expected = std::str::from_utf8(fixture).unwrap();
    assert_eq!(
        actual,
        Some(expected),
        "[{label}] reconstruction failed; raw zsh stdout = {stdout:02X?}"
    );
    assert_eq!(
        p.state().cwd().cloned(),
        cwd_before,
        "[{label}] OSC 7772 dispatch must not change cwd"
    );
}

#[test]
fn osc7772_real_zsh_roundtrip() {
    if !zsh_available() {
        eprintln!("skipping osc7772_real_zsh_roundtrip: zsh not on PATH");
        return;
    }

    // Each fixture exercises a different failure mode of the legacy
    // raw 7770 framing: ';' splitting, BEL early-terminate, ESC[…m
    // colour codes, multi-byte UTF-8, and a deliberate OSC 7 smuggle.
    assert_roundtrips("plain semicolon", b"echo a; ls -la");
    assert_roundtrips("compound", b"if true; then echo a; fi");
    assert_roundtrips("bel inside", b"x\x07y");
    assert_roundtrips("ansi colour", b"\x1b[31mred\x1b[0m");
    assert_roundtrips("cjk + semicolon", "日本語; cmd".as_bytes());

    // Smuggle attempt: the buffer LOOKS like OSC 7 (CWD update). After
    // round-trip the `cwd` MUST remain unchanged — the decoded bytes go
    // straight to `set_command_buffer`, not back through the VTE parser.
    assert_roundtrips("osc7 smuggle attempt", b"\x1b]7;file:///etc/passwd\x07");
}
