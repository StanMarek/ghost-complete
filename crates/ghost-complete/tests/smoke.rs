mod harness;

use harness::GhostProcess;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const OSC_GIT_REPORT: &[u8] = b"\x1b]7770;4;git \x07";
const POPUP_RENDER_MARKER: &[u8] = b"\x1b7";
const POPUP_RESTORE_MARKER: &[u8] = b"\x1b8";
const REDRAW_MARKER: &[u8] = b"@";
const NO_POPUP_BEFORE_REDRAW_TIMEOUT: Duration = Duration::from_millis(500);
const NO_POPUP_DURING_DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(300);
const OSC_ONLY_DEBOUNCE_DELAY_MS: u64 = 1000;

trait PopupSmokeProcess {
    fn wait_for_bytes_after(&self, needle: &[u8], start_offset: usize, timeout: Duration) -> bool;
    fn output_snapshot(&self) -> Vec<u8>;
    fn output_len(&self) -> usize;
    fn write_raw(&mut self, data: &[u8]);
}

impl PopupSmokeProcess for GhostProcess {
    fn wait_for_bytes_after(&self, needle: &[u8], start_offset: usize, timeout: Duration) -> bool {
        GhostProcess::wait_for_bytes_after(self, needle, start_offset, timeout)
    }

    fn output_snapshot(&self) -> Vec<u8> {
        GhostProcess::output_snapshot(self)
    }

    fn output_len(&self) -> usize {
        GhostProcess::output_len(self)
    }

    fn write_raw(&mut self, data: &[u8]) {
        GhostProcess::write_raw(self, data);
    }
}

struct ConfiguredGhostProcess {
    writer: Box<dyn Write + Send>,
    output: Arc<(Mutex<Vec<u8>>, Condvar)>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _config: tempfile::NamedTempFile,
}

impl ConfiguredGhostProcess {
    fn spawn_with_config(config: &str) -> Self {
        let mut config_file = tempfile::NamedTempFile::new().expect("failed to create temp config");
        config_file
            .write_all(config.as_bytes())
            .expect("failed to write temp config");
        config_file.flush().expect("failed to flush temp config");
        let config_path = config_file
            .path()
            .to_str()
            .expect("temp config path must be valid UTF-8");

        let pty_system = native_pty_system();
        let pty_pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("failed to open PTY pair");

        let bin = env!("CARGO_BIN_EXE_ghost-complete");
        let mut cmd = CommandBuilder::new(bin);
        cmd.args(["--config", config_path, "--log-level", "error", "/bin/sh"]);
        cmd.env("TERM_PROGRAM", "ghostty");

        let child = pty_pair
            .slave
            .spawn_command(cmd)
            .expect("failed to spawn ghost-complete");

        let writer = pty_pair
            .master
            .take_writer()
            .expect("failed to take PTY writer");
        let mut reader = pty_pair
            .master
            .try_clone_reader()
            .expect("failed to clone PTY reader");

        let output = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let output_clone = Arc::clone(&output);
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let (lock, cvar) = &*output_clone;
                        let mut data = lock.lock().unwrap();
                        data.extend_from_slice(&buf[..n]);
                        cvar.notify_all();
                    }
                    Err(_) => break,
                }
            }
        });

        thread::sleep(Duration::from_millis(1500));

        Self {
            writer,
            output,
            child,
            _config: config_file,
        }
    }

    fn send_line(&mut self, line: &str) {
        let data = format!("{}\r", line);
        self.writer
            .write_all(data.as_bytes())
            .expect("failed to write to PTY");
        self.writer.flush().expect("failed to flush PTY writer");
    }

    fn expect_output(&self, substr: &str) {
        let timeout = Duration::from_secs(10);
        let start = Instant::now();
        let (lock, cvar) = &*self.output;

        loop {
            let data = lock.lock().unwrap();
            let text = String::from_utf8_lossy(&data);
            if text.contains(substr) {
                return;
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                panic!(
                    "Timed out after {:?} waiting for {:?} in output.\nOutput so far ({} bytes):\n{}",
                    timeout,
                    substr,
                    data.len(),
                    String::from_utf8_lossy(&data[..data.len().min(2000)])
                );
            }
            let remaining = timeout - elapsed;
            let (data, _) = cvar.wait_timeout(data, remaining).unwrap();
            let text = String::from_utf8_lossy(&data);
            if text.contains(substr) {
                return;
            }
        }
    }

    fn exit_with_code(&mut self, code: i32) -> i32 {
        self.send_line(&format!("exit {}", code));
        self.wait_for_exit()
    }

    fn wait_for_exit(&mut self) -> i32 {
        let timeout = Duration::from_secs(15);
        let start = Instant::now();

        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait failed") {
                return status.exit_code().try_into().unwrap_or(1);
            }
            if start.elapsed() >= timeout {
                self.child.kill().ok();
                panic!("Process did not exit within {:?}", timeout);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl PopupSmokeProcess for ConfiguredGhostProcess {
    fn wait_for_bytes_after(&self, needle: &[u8], start_offset: usize, timeout: Duration) -> bool {
        let start = Instant::now();
        let (lock, cvar) = &*self.output;
        loop {
            let data = lock.lock().unwrap();
            if data.len() > start_offset && find_subslice(&data[start_offset..], needle).is_some() {
                return true;
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return false;
            }
            let remaining = timeout - elapsed;
            let (data, _) = cvar.wait_timeout(data, remaining).unwrap();
            if data.len() > start_offset && find_subslice(&data[start_offset..], needle).is_some() {
                return true;
            }
        }
    }

    fn output_snapshot(&self) -> Vec<u8> {
        let (lock, _) = &*self.output;
        lock.lock().unwrap().clone()
    }

    fn output_len(&self) -> usize {
        let (lock, _) = &*self.output;
        lock.lock().unwrap().len()
    }

    fn write_raw(&mut self, data: &[u8]) {
        self.writer
            .write_all(data)
            .expect("failed to write raw to PTY");
        self.writer.flush().expect("failed to flush PTY writer");
    }
}

impl Drop for ConfiguredGhostProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.child.kill().ok();
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn assert_osc_only_report_defers_popup<P: PopupSmokeProcess + ?Sized>(
    proc: &P,
    start_offset: usize,
    context: &str,
) {
    let osc_seen = proc.wait_for_bytes_after(OSC_GIT_REPORT, start_offset, Duration::from_secs(5));
    if !osc_seen {
        let snapshot = proc.output_snapshot();
        let since_start = &snapshot[start_offset..];
        panic!(
            "OSC 7770 report did not arrive before {context}.\n\
             Bytes since mark ({} bytes, lossy UTF-8):\n{:?}",
            since_start.len(),
            String::from_utf8_lossy(since_start),
        );
    }

    let rendered_before_redraw = proc.wait_for_bytes_after(
        POPUP_RENDER_MARKER,
        start_offset,
        NO_POPUP_BEFORE_REDRAW_TIMEOUT,
    );
    if rendered_before_redraw {
        let snapshot = proc.output_snapshot();
        let since_start = &snapshot[start_offset..];
        panic!(
            "OSC-only buffer report rendered popup before later display change during {context}.\n\
             Bytes since mark ({} bytes, lossy UTF-8):\n{:?}",
            since_start.len(),
            String::from_utf8_lossy(since_start),
        );
    }
}

fn advance_display_after_osc_only_report<P: PopupSmokeProcess + ?Sized>(proc: &mut P) -> usize {
    let mark_before_redraw = proc.output_len();
    // The shell is blocked in `read`; one printable byte is echoed by the PTY
    // without completing the read, giving the proxy a real later display epoch.
    proc.write_raw(REDRAW_MARKER);
    mark_before_redraw
}

fn wait_for_output_quiet<P: PopupSmokeProcess + ?Sized>(
    proc: &P,
    quiet_for: Duration,
    timeout: Duration,
) {
    let start = Instant::now();
    let mut quiet_start = Instant::now();
    let mut previous_len = proc.output_len();

    loop {
        thread::sleep(Duration::from_millis(25));
        let current_len = proc.output_len();
        if current_len == previous_len {
            if quiet_start.elapsed() >= quiet_for {
                return;
            }
        } else {
            previous_len = current_len;
            quiet_start = Instant::now();
        }

        if start.elapsed() >= timeout {
            let snapshot = proc.output_snapshot();
            panic!(
                "Output did not stay quiet for {:?} within {:?}.\n\
                 Output so far ({} bytes, lossy UTF-8):\n{:?}",
                quiet_for,
                timeout,
                snapshot.len(),
                String::from_utf8_lossy(&snapshot),
            );
        }
    }
}

fn assert_redraw_marker_precedes_popup<P: PopupSmokeProcess + ?Sized>(
    proc: &P,
    mark_before_redraw: usize,
    context: &str,
) {
    let snapshot = proc.output_snapshot();
    let since_redraw = &snapshot[mark_before_redraw..];
    let redraw_pos = find_subslice(since_redraw, REDRAW_MARKER).unwrap_or_else(|| {
        panic!(
            "Redraw marker missing after display advance during {context}.\n\
             Bytes since redraw mark ({} bytes, lossy UTF-8):\n{:?}",
            since_redraw.len(),
            String::from_utf8_lossy(since_redraw),
        )
    });
    let popup_pos = find_subslice(since_redraw, POPUP_RENDER_MARKER).unwrap_or_else(|| {
        panic!(
            "Popup marker missing after display advance during {context}.\n\
             Bytes since redraw mark ({} bytes, lossy UTF-8):\n{:?}",
            since_redraw.len(),
            String::from_utf8_lossy(since_redraw),
        )
    });

    assert!(
        redraw_pos < popup_pos,
        "display output must be forwarded before popup render during {context}. \
         Redraw marker at {}, popup marker at {}.\n\
         Bytes since redraw mark ({} bytes, lossy UTF-8):\n{:?}",
        redraw_pos,
        popup_pos,
        since_redraw.len(),
        String::from_utf8_lossy(since_redraw),
    );
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
/// shell integration) -> defer until the terminal display advances -> popup
/// renders with git-spec subcommand text -> ESC dismisses the popup.
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
///   `buffer_dirty = true`, which Task B (stdout -> terminal loop) notices.
///   OSC-only reports are intentionally deferred until later display output
///   proves the terminal has advanced/redrawn after the report.
/// - No manual Ctrl+/ is needed in this test; emitting an OSC buffer report
///   directly exercises Task B's auto-trigger/debounce path. zsh production
///   integration uses OSC 7772 from zle-line-pre-redraw, while OSC 7770
///   remains accepted for legacy compatibility and is convenient for this
///   smoke harness.
///
/// Assumptions:
///   - Embedded `git` spec declares well-known subcommands such as status,
///     commit, branch, checkout, clone, and add, and is always embedded via
///     include_str!.
///   - Trigger char ' ' is in the default `auto_chars` list, so the space
///     at the end of "git " activates the auto-trigger path.
///   - `clear_popup` emits DECSC (`\x1b7`) followed by blanking writes and
///     DECRC (`\x1b8`). DECRC appearing after our ESC mark = dismissed.
///   - Default dismiss keybind is ESC (see Keybindings::default()).
///
/// Determinism: positive readiness waits use byte-level polling with
/// condvar-based wakeups and bounded timeouts. Startup settling and negative
/// guards still use bounded sleeps/timeouts where the test needs to prove
/// absence before redraw.
#[test]
fn test_popup_renders_and_dismisses_on_git_trigger() {
    let mut proc = GhostProcess::spawn();

    // Settle the shell so our printf doesn't race with any banner output.
    proc.send_line("printf '%s%s\\n' smoke_popup_ready_ marker");
    proc.expect_output("smoke_popup_ready_marker");

    // Mark the pre-trigger offset — popup render bytes must appear after.
    let mark_before_trigger = proc.output_len();

    // Inject OSC 7770 via shell printf. Format:
    //     OSC ] 7770 ; <cursor-char-offset> ; <buffer> BEL
    //     \x1b]7770;4;git \x07
    // The shell executes printf, emits the raw bytes to stdout, gc-parser
    // consumes them and sets command_buffer = "git " with cursor = 4.
    // The buffer_dirty flag causes Task B to defer the trigger until a later
    // display epoch.
    //
    // Keep the shell command blocked after the OSC write. If the command
    // exits immediately, the shell prints a new prompt, and the proxy now
    // correctly tears down the popup before forwarding that prompt output.
    proc.send_line(r"printf '\033]7770;4;git \007'; read _ghost_popup_hold");

    assert_osc_only_report_defers_popup(&proc, mark_before_trigger, "git popup trigger");

    let mark_before_redraw = advance_display_after_osc_only_report(&mut proc);

    // A popup render includes DECSC (`\x1b7` = save cursor). Seeing it after
    // mark_before_redraw proves that the OSC 7770 -> later display epoch ->
    // render_popup pipeline ran.
    let popup_rendered = proc.wait_for_bytes_after(
        POPUP_RENDER_MARKER,
        mark_before_redraw,
        Duration::from_secs(5),
    );

    if !popup_rendered {
        let snapshot = proc.output_snapshot();
        let since_redraw = &snapshot[mark_before_redraw..];
        panic!(
            "Popup did not render within 5s after display advanced following OSC 7770 injection.\n\
             Bytes since redraw mark ({} bytes, lossy UTF-8):\n{:?}",
            since_redraw.len(),
            String::from_utf8_lossy(since_redraw),
        );
    }
    assert_redraw_marker_precedes_popup(&proc, mark_before_redraw, "git popup trigger");

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
    let dismissed = proc.wait_for_bytes_after(
        POPUP_RESTORE_MARKER,
        mark_before_esc,
        Duration::from_secs(5),
    );

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
fn test_osc_only_report_without_pending_trigger_waits_for_debounce_after_redraw() {
    let config = format!("[trigger]\ndelay_ms = {}\n", OSC_ONLY_DEBOUNCE_DELAY_MS);
    let mut proc = ConfiguredGhostProcess::spawn_with_config(&config);

    proc.send_line("echo${IFS}smoke_debounce_ready");
    proc.expect_output("smoke_debounce_ready");
    wait_for_output_quiet(&proc, Duration::from_millis(150), Duration::from_secs(5));

    let mark_before_trigger = proc.output_len();

    // This shell command contains no literal default auto-trigger chars
    // (space, '/', '-', '.') before the OSC report. `${IFS}` supplies shell
    // separators at execution time, and `\040` emits the buffer's trailing
    // space from printf rather than from typed input.
    proc.send_line(r"printf${IFS}'\033]7770;4;git\040\007';read${IFS}_gc_smoke_gate");

    assert_osc_only_report_defers_popup(&proc, mark_before_trigger, "no-pending debounce");

    let mark_before_redraw = advance_display_after_osc_only_report(&mut proc);
    let redraw_seen =
        proc.wait_for_bytes_after(REDRAW_MARKER, mark_before_redraw, Duration::from_secs(5));
    if !redraw_seen {
        let snapshot = proc.output_snapshot();
        let since_redraw = &snapshot[mark_before_redraw..];
        panic!(
            "Redraw marker did not arrive after OSC-only report.\n\
             Bytes since redraw mark ({} bytes, lossy UTF-8):\n{:?}",
            since_redraw.len(),
            String::from_utf8_lossy(since_redraw),
        );
    }

    let snapshot_after_redraw = proc.output_snapshot();
    let since_redraw = &snapshot_after_redraw[mark_before_redraw..];
    if find_subslice(since_redraw, POPUP_RENDER_MARKER).is_some() {
        panic!(
            "OSC-only buffer report rendered immediately when the later display output arrived; \
             expected deferred debounce.\n\
             Bytes since redraw mark ({} bytes, lossy UTF-8):\n{:?}",
            since_redraw.len(),
            String::from_utf8_lossy(since_redraw),
        );
    }

    let mark_after_redraw = snapshot_after_redraw.len();
    let rendered_during_debounce = proc.wait_for_bytes_after(
        POPUP_RENDER_MARKER,
        mark_after_redraw,
        NO_POPUP_DURING_DEBOUNCE_TIMEOUT,
    );
    if rendered_during_debounce {
        let snapshot = proc.output_snapshot();
        let during_debounce = &snapshot[mark_after_redraw..];
        panic!(
            "OSC-only buffer report rendered before the configured debounce elapsed.\n\
             Bytes during debounce guard ({} bytes, lossy UTF-8):\n{:?}",
            during_debounce.len(),
            String::from_utf8_lossy(during_debounce),
        );
    }

    let popup_rendered = proc.wait_for_bytes_after(
        POPUP_RENDER_MARKER,
        mark_after_redraw,
        Duration::from_secs(5),
    );
    if !popup_rendered {
        let snapshot = proc.output_snapshot();
        let since_redraw = &snapshot[mark_before_redraw..];
        panic!(
            "Popup did not render after deferred debounce following OSC-only report.\n\
             Bytes since redraw mark ({} bytes, lossy UTF-8):\n{:?}",
            since_redraw.len(),
            String::from_utf8_lossy(since_redraw),
        );
    }
    assert_redraw_marker_precedes_popup(&proc, mark_before_redraw, "no-pending debounce");

    proc.send_line("");
    proc.exit_with_code(0);
}

#[test]
fn test_popup_is_cleared_before_later_shell_output() {
    let mut proc = GhostProcess::spawn();

    proc.send_line("PS1=\"smoke_prompt_repaint_$((1+1))_marker \"");
    proc.expect_output("smoke_prompt_repaint_2_marker");

    let mark_before_trigger = proc.output_len();
    proc.send_line(r"printf '\033]7770;4;git \007'; read _gc_smoke_gate");

    assert_osc_only_report_defers_popup(&proc, mark_before_trigger, "prompt repaint cleanup");

    let mark_before_redraw = advance_display_after_osc_only_report(&mut proc);

    let popup_rendered = proc.wait_for_bytes_after(
        POPUP_RENDER_MARKER,
        mark_before_redraw,
        Duration::from_secs(5),
    );
    if !popup_rendered {
        let snapshot = proc.output_snapshot();
        let since_redraw = &snapshot[mark_before_redraw..];
        panic!(
            "Popup did not render after display advanced before prompt repaint.\n\
             Bytes since redraw mark ({} bytes, lossy UTF-8):\n{:?}",
            since_redraw.len(),
            String::from_utf8_lossy(since_redraw),
        );
    }
    assert_redraw_marker_precedes_popup(&proc, mark_before_redraw, "prompt repaint cleanup");

    let mark_after_popup = proc.output_len();
    let marker = b"smoke_prompt_repaint_2_marker";
    proc.send_line("");
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
        find_subslice(before_marker, POPUP_RESTORE_MARKER).is_some(),
        "popup cleanup must finish before later shell output is forwarded. \
         Bytes before marker ({} bytes, lossy UTF-8):\n{:?}",
        before_marker.len(),
        String::from_utf8_lossy(before_marker),
    );

    let after_marker = &since_popup[marker_pos + marker.len()..];
    assert!(
        find_subslice(after_marker, POPUP_RENDER_MARKER).is_none(),
        "no stale popup render should follow shell repaint output. \
         Bytes after marker ({} bytes, lossy UTF-8):\n{:?}",
        after_marker.len(),
        String::from_utf8_lossy(after_marker),
    );

    proc.exit_with_code(0);
}
