mod harness;

use harness::GhostProcess;
use std::thread;
use std::time::Duration;

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[test]
fn test_echo_passthrough() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo hello_smoke_test");
    proc.expect_output("hello_smoke_test");
    proc.exit_with_code(0);
}

#[test]
fn test_exit_code_zero() {
    let mut proc = GhostProcess::spawn();
    let code = proc.exit_with_code(0);
    assert_eq!(code, 0, "expected exit code 0, got {}", code);
}

#[test]
fn test_exit_code_nonzero() {
    let mut proc = GhostProcess::spawn();
    let code = proc.exit_with_code(42);
    assert_eq!(code, 42, "expected exit code 42, got {}", code);
}

#[test]
fn test_large_output() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("seq 1 5000");
    // Wait for a number that appears only in seq's output (not in the echoed command).
    // "5000" also appears in "seq 1 5000", so we wait for "4999" instead.
    proc.expect_output("4999");

    // Poll until output buffer has stabilized (no new bytes for 500ms).
    let mut prev_len = 0;
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(500));
        let snapshot = proc.output_snapshot();
        if snapshot.len() == prev_len {
            break;
        }
        prev_len = snapshot.len();
    }

    let snapshot = proc.output_snapshot();
    let text = String::from_utf8_lossy(&snapshot);
    // Check a spread of numbers. Use numbers > 4 digits to avoid false positives
    // from ANSI escape sequence parameters (e.g. "\x1b[100;1H" cursor positioning).
    for n in &[1000, 2500, 3333, 4999, 5000] {
        let needle = format!("{}", n);
        assert!(
            text.contains(&needle),
            "large output missing expected number {} (output {} bytes)",
            n,
            snapshot.len()
        );
    }
    proc.exit_with_code(0);
}

#[test]
fn test_environment_preserved() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo HOME_IS=$HOME");
    proc.expect_output("HOME_IS=/");
    proc.exit_with_code(0);
}

#[test]
fn test_pipe_passthrough() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo pipe_marker | cat");
    proc.expect_output("pipe_marker");
    proc.exit_with_code(0);
}

#[test]
fn test_rapid_input() {
    let mut proc = GhostProcess::spawn();
    for i in 0..20 {
        proc.send_line(&format!("echo rapid_{}", i));
    }
    proc.expect_output("rapid_19");

    let snapshot = proc.output_snapshot();
    let text = String::from_utf8_lossy(&snapshot);
    assert!(text.contains("rapid_0"), "missing rapid_0 in output");
    assert!(text.contains("rapid_10"), "missing rapid_10 in output");
    proc.exit_with_code(0);
}

#[test]
fn test_memory_baseline() {
    let proc = GhostProcess::spawn();
    thread::sleep(Duration::from_secs(1));

    if let Some(pid) = proc.child_pid() {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .expect("failed to run ps");
        let rss_str = String::from_utf8_lossy(&output.stdout);
        if let Ok(rss_kb) = rss_str.trim().parse::<u64>() {
            let rss_mb = rss_kb / 1024;
            assert!(
                rss_mb < 500,
                "RSS is {} MB, exceeds 500 MB threshold",
                rss_mb
            );
        }
        // If we can't parse RSS (process already exited), that's fine — skip the check.
    }
}

#[test]
fn test_clean_startup_shutdown() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo alive");
    proc.expect_output("alive");
    let code = proc.exit_with_code(0);
    assert_eq!(code, 0, "expected clean exit 0, got {}", code);
}

#[test]
fn test_multiple_commands() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo aaa");
    proc.expect_output("aaa");
    proc.send_line("echo bbb");
    proc.expect_output("bbb");
    proc.send_line("echo ccc");
    proc.expect_output("ccc");
    proc.exit_with_code(0);
}

/// End-to-end popup smoke test.
///
/// Verifies the entire UX pipeline: OSC 7770 buffer-report (from simulated
/// shell integration) -> auto-trigger -> popup renders with git-spec
/// subcommand text -> ESC dismisses the popup.
///
/// Architecture notes:
/// - The harness wraps `/bin/sh` (no shell integration). Without shell
///   integration, the shell will NOT emit OSC 7770 buffer-report sequences,
///   so the parser's `command_buffer` stays empty, and `handler.trigger()`
///   would dismiss immediately (see gc-pty/src/handler.rs: `if
///   buffer.is_empty() { return; }`).
/// - To simulate shell integration without installing it, we have the
///   inner shell print OSC 7770 itself: `printf '\033]7770;4;git \007'`.
///   The shell executes printf, emits the raw ANSI bytes to its stdout,
///   which flow through gc-parser's VT state machine and set
///   `command_buffer = "git "` with cursor = 4. This also sets
///   `buffer_dirty = true`, which Task B (stdout -> terminal loop) notices
///   and uses to fire `trigger()` automatically.
/// - No manual Ctrl+/ is needed — the auto-trigger path from OSC 7770 is
///   exactly what real shell integration does on every keystroke.
///
/// Assumptions:
///   - Embedded `git` spec contains well-known subcommands (status, commit,
///     branch, etc.). Verified: specs/git.json has ~30 `"name": "status"`
///     occurrences and is always embedded via include_str!.
///   - Trigger char ' ' is in the default `auto_chars` list, so the space
///     at the end of "git " activates the auto-trigger path.
///   - `clear_popup` emits DECSC (`\x1b7`) followed by blanking writes and
///     DECRC (`\x1b8`). DECRC appearing after our ESC mark = dismissed.
///   - Default dismiss keybind is ESC (see Keybindings::default()).
///
/// Determinism: byte-level polling with condvar-based wakeup, 5s timeouts,
/// no blind sleeps.
#[test]
fn test_popup_renders_and_dismisses_on_git_trigger() {
    let mut proc = GhostProcess::spawn();

    // Settle the shell so our printf doesn't race with any banner output.
    proc.send_line("echo smoke_popup_ready_marker");
    proc.expect_output("smoke_popup_ready_marker");

    // Mark the pre-trigger offset — popup render bytes must appear after.
    let mark_before_trigger = proc.output_len();

    // Inject OSC 7770 via shell printf. Format:
    //     OSC ] 7770 ; <cursor-char-offset> ; <buffer> BEL
    //     \x1b]7770;4;git \x07
    // The shell executes printf, emits the raw bytes to stdout, gc-parser
    // consumes them and sets command_buffer = "git " with cursor = 4.
    // The buffer_dirty flag causes Task B to auto-fire trigger().
    //
    // Keep the shell command blocked after the OSC write. If the command
    // exits immediately, the shell prints a new prompt, and the proxy now
    // correctly tears down the popup before forwarding that prompt output.
    proc.send_line(r"printf '\033]7770;4;git \007'; read _ghost_popup_hold");

    // Wait for the popup render. The overlay's first emitted byte sequence
    // is DECSC (`\x1b7` = save cursor). Seeing it after mark_before_trigger
    // proves that the OSC 7770 -> auto-trigger -> render_popup pipeline ran.
    let popup_rendered =
        proc.wait_for_bytes_after(b"\x1b7", mark_before_trigger, Duration::from_secs(5));

    if !popup_rendered {
        let snapshot = proc.output_snapshot();
        let since_trigger = &snapshot[mark_before_trigger..];
        panic!(
            "Popup did not render within 5s after OSC 7770 injection.\n\
             Bytes since trigger mark ({} bytes, lossy UTF-8):\n{:?}",
            since_trigger.len(),
            String::from_utf8_lossy(since_trigger),
        );
    }

    // Assert the popup contains git subcommand text. We accept any of a
    // handful of well-known git subcommands because the exact ordering on
    // the first page depends on nucleo's empty-query ordering and the
    // spec's declared order. Tolerating several known-good names keeps the
    // test deterministic across future spec updates.
    let snapshot_after_popup = proc.output_snapshot();
    let popup_slice = &snapshot_after_popup[mark_before_trigger..];
    let popup_text = String::from_utf8_lossy(popup_slice);

    let git_subcommands = ["status", "commit", "branch", "checkout", "clone", "add"];
    let found: Vec<&&str> = git_subcommands
        .iter()
        .filter(|needle| popup_text.contains(**needle))
        .collect();

    assert!(
        !found.is_empty(),
        "Popup rendered (DECSC seen) but no git-spec subcommand found in its output. \
         Expected one of {:?}.\nPopup slice ({} bytes, lossy UTF-8):\n{:?}",
        git_subcommands,
        popup_slice.len(),
        popup_text,
    );

    // Mark offset before dismiss — dismissal bytes must appear after.
    let mark_before_esc = proc.output_len();

    // Send a lone ESC (0x1B). The input parser treats a lone ESC at end
    // of buffer as KeyEvent::Escape, which dispatches dismiss().
    proc.write_raw(&[0x1B]);

    // clear_popup emits DECSC + movement + blanks + DECRC. The DECRC
    // (`\x1b8`) appearing after the ESC mark is the dismiss signal.
    let dismissed = proc.wait_for_bytes_after(b"\x1b8", mark_before_esc, Duration::from_secs(5));

    if !dismissed {
        let snapshot = proc.output_snapshot();
        let since_esc = &snapshot[mark_before_esc..];
        panic!(
            "Popup did not dismiss within 5s after ESC.\n\
             Bytes since ESC mark ({} bytes, lossy UTF-8):\n{:?}",
            since_esc.len(),
            String::from_utf8_lossy(since_esc),
        );
    }

    // Release the blocking `read` used to keep shell output from racing the
    // visible-popup dismissal path.
    proc.send_line("");

    proc.exit_with_code(0);
}

#[test]
fn test_popup_is_cleared_before_later_shell_output() {
    let mut proc = GhostProcess::spawn();

    proc.send_line("echo smoke_popup_repaint_ready_marker");
    proc.expect_output("smoke_popup_repaint_ready_marker");

    let mark_before_trigger = proc.output_len();
    proc.send_line(r"printf '\033]7770;4;git \007'; sleep 1; echo smoke_prompt_repaint_marker");

    let popup_rendered =
        proc.wait_for_bytes_after(b"\x1b7", mark_before_trigger, Duration::from_secs(5));
    if !popup_rendered {
        let snapshot = proc.output_snapshot();
        let since_trigger = &snapshot[mark_before_trigger..];
        panic!(
            "Popup did not render before prompt repaint.\n\
             Bytes since trigger mark ({} bytes, lossy UTF-8):\n{:?}",
            since_trigger.len(),
            String::from_utf8_lossy(since_trigger),
        );
    }

    let mark_after_popup = proc.output_len();
    let marker = b"smoke_prompt_repaint_marker";
    let marker_seen = proc.wait_for_bytes_after(marker, mark_after_popup, Duration::from_secs(5));
    if !marker_seen {
        let snapshot = proc.output_snapshot();
        let since_popup = &snapshot[mark_after_popup..];
        panic!(
            "Shell repaint marker did not arrive after popup render.\n\
             Bytes since popup mark ({} bytes, lossy UTF-8):\n{:?}",
            since_popup.len(),
            String::from_utf8_lossy(since_popup),
        );
    }

    let snapshot = proc.output_snapshot();
    let since_popup = &snapshot[mark_after_popup..];
    let marker_pos = find_subslice(since_popup, marker).expect("marker position");
    let before_marker = &since_popup[..marker_pos];
    assert!(
        find_subslice(before_marker, b"\x1b8").is_some(),
        "popup cleanup must finish before later shell output is forwarded. \
         Bytes before marker ({} bytes, lossy UTF-8):\n{:?}",
        before_marker.len(),
        String::from_utf8_lossy(before_marker),
    );

    let after_marker = &since_popup[marker_pos + marker.len()..];
    assert!(
        find_subslice(after_marker, b"\x1b7").is_none(),
        "no stale popup render should follow shell repaint output. \
         Bytes after marker ({} bytes, lossy UTF-8):\n{:?}",
        after_marker.len(),
        String::from_utf8_lossy(after_marker),
    );

    proc.exit_with_code(0);
}
