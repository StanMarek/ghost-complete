//! Regression tests for #185: when the terminal hosting the proxy goes away
//! (window or tab closed), the proxy must hang up its shell and exit, the way
//! a terminal emulator would — never leak, never act on pending input.
//!
//! The "terminal" here is a PTY master owned by the test. The test talks to it
//! through its raw fd only: `MasterPty::try_clone_reader` and `take_writer`
//! both dup the fd, so a reader thread parked in `read` would keep the
//! terminal open after `close_terminal`, and dropping a portable-pty writer
//! types `\n` + VEOF into the proxy's stdin, which is itself a way to submit
//! the command line.

use std::os::fd::RawFd;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// How long the proxy may take to exit once its terminal is gone. A shell
/// that exits on SIGHUP takes well under a second; one that ignores it is
/// killed after the proxy's 2 s grace. The bug was "never".
const EXIT_DEADLINE: Duration = Duration::from_secs(5);

/// Debug builds decompress embedded specs at startup; leave generous room.
const PROMPT_DEADLINE: Duration = Duration::from_secs(15);

/// zsh imports `PS1` from the environment, and `-o promptsubst` expands `$$`,
/// so the first prompt tells us the shell's pid.
const PROMPT_PREFIX: &[u8] = b"gc-ready:";

struct Session {
    master: Option<Box<dyn MasterPty + Send>>,
    fd: RawFd,
    proxy: Box<dyn Child + Send + Sync>,
    shell_pid: libc::pid_t,
}

impl Session {
    /// Start `ghost-complete -- /bin/zsh -f` on a fresh PTY, with `home` as
    /// its cwd and `$HOME`, and wait for the shell's first prompt, so the
    /// proxy's I/O tasks and signal handlers are all live before the test
    /// pulls the terminal away.
    fn spawn(home: &Path) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("failed to open PTY pair");

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ghost-complete"));
        cmd.args([
            "--log-file",
            "/dev/null",
            "--",
            "/bin/zsh",
            "-f",
            "-o",
            "promptsubst",
        ]);
        cmd.cwd(home);
        // Keep the proxy's config, spec mirror and frecency store out of the
        // developer's real home.
        cmd.env("HOME", home);
        for var in [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
        ] {
            cmd.env_remove(var);
        }
        cmd.env("TERM", "xterm-256color");
        // A known terminal, so the proxy doesn't fall back to exec'ing zsh.
        cmd.env("TERM_PROGRAM", "ghostty");
        cmd.env("PS1", "gc-ready:$$> ");
        cmd.env_remove("GHOST_COMPLETE_ACTIVE");
        // Keep the proxy from `tmux setenv`-ing into the developer's session.
        cmd.env_remove("TMUX");
        cmd.env_remove("TMUX_PANE");

        let proxy = pair
            .slave
            .spawn_command(cmd)
            .expect("failed to spawn ghost-complete");
        // Only the proxy may hold the slave, or closing the master is not a
        // hangup.
        drop(pair.slave);

        let master = pair.master;
        let fd = master.as_raw_fd().expect("PTY master has no raw fd");
        let mut session = Session {
            master: Some(master),
            fd,
            proxy,
            shell_pid: 0,
        };
        let output = session.read_until(PROMPT_DEADLINE, |out| shell_pid(out).is_some());
        session.shell_pid = shell_pid(&output).expect("read_until returned without a prompt");
        session
    }

    /// Type `text` at the prompt without pressing Enter, and wait for the
    /// shell to echo it back so it is sitting in zsh's line editor.
    fn type_without_enter(&mut self, text: &str) {
        let mut bytes = text.as_bytes();
        while !bytes.is_empty() {
            // SAFETY: `self.fd` is the open master fd owned by `self.master`.
            let n = unsafe { libc::write(self.fd, bytes.as_ptr().cast(), bytes.len()) };
            assert!(
                n > 0,
                "write to PTY master failed: {}",
                std::io::Error::last_os_error()
            );
            bytes = &bytes[n as usize..];
        }
        let tail = &text.as_bytes()[text.len().saturating_sub(8)..];
        self.read_until(PROMPT_DEADLINE, |out| contains(out, tail));
    }

    /// Simulate the terminal window closing: close the only master fd.
    fn close_terminal(&mut self) {
        drop(self.master.take());
    }

    /// Poll until the proxy exits. Returns `false` if it outlives `deadline`.
    fn wait_for_proxy_exit(&mut self, deadline: Duration) -> bool {
        let start = Instant::now();
        loop {
            if self.proxy.try_wait().expect("try_wait failed").is_some() {
                return true;
            }
            if start.elapsed() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn read_until(&self, deadline: Duration, mut done: impl FnMut(&[u8]) -> bool) -> Vec<u8> {
        let end = Instant::now() + deadline;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while !done(&out) {
            let remaining = end.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out after {deadline:?}; output so far:\n{}",
                String::from_utf8_lossy(&out)
            );
            let mut pfd = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            // SAFETY: `pfd` is a valid pollfd for the duration of the call.
            if unsafe { libc::poll(&mut pfd, 1, timeout_ms) } <= 0 {
                continue; // timeout or EINTR; the deadline check above decides
            }
            // SAFETY: `buf` is writable for `buf.len()` bytes.
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            assert!(
                n > 0,
                "terminal closed before the expected output; output so far:\n{}",
                String::from_utf8_lossy(&out)
            );
            out.extend_from_slice(&buf[..n as usize]);
        }
        out
    }
}

impl Drop for Session {
    /// Never leave a leaked proxy or shell behind, whatever the test saw.
    fn drop(&mut self) {
        if self.shell_pid > 0 {
            // SAFETY: plain kill(2); ESRCH (already gone) is fine.
            unsafe { libc::kill(self.shell_pid, libc::SIGKILL) };
        }
        if self.proxy.try_wait().ok().flatten().is_none() {
            let _ = self.proxy.kill();
            let _ = self.proxy.wait();
        }
    }
}

/// Parse the shell pid out of the first `gc-ready:<pid>> ` prompt.
fn shell_pid(output: &[u8]) -> Option<libc::pid_t> {
    let start = output
        .windows(PROMPT_PREFIX.len())
        .position(|w| w == PROMPT_PREFIX)?
        + PROMPT_PREFIX.len();
    let rest = &output[start..];
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || rest.get(digits) != Some(&b'>') {
        return None;
    }
    std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Poll until `pid` no longer exists. A proxy that exits without reaping its
/// shell leaves it alive under launchd, which this catches.
fn process_is_gone(pid: libc::pid_t, deadline: Duration) -> bool {
    let start = Instant::now();
    loop {
        // SAFETY: signal 0 only probes for existence.
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
        if !alive {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn proxy_hangs_up_shell_and_exits_when_terminal_closes() {
    // Closing the terminal readies both the I/O-finished and the SIGHUP
    // branches of the proxy's `select!`, and which one wins is random. Before
    // the fix, the I/O branch blocked forever in `child.wait()` (most runs
    // leaked), so a single pass proves little.
    for attempt in 1..=5 {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = Session::spawn(dir.path());

        session.close_terminal();

        assert!(
            session.wait_for_proxy_exit(EXIT_DEADLINE),
            "attempt {attempt}: proxy still running {EXIT_DEADLINE:?} after its terminal closed"
        );
        assert!(
            process_is_gone(session.shell_pid, Duration::from_secs(2)),
            "attempt {attempt}: proxy exited but its shell (pid {}) is still alive",
            session.shell_pid
        );
    }
}

#[test]
fn terminal_close_does_not_submit_half_typed_command() {
    // A terminal emulator closing its window hangs up the shell; whatever is
    // on the command line is discarded, never executed.
    for attempt in 1..=3 {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("submitted-on-close");
        let mut session = Session::spawn(dir.path());
        session.type_without_enter(&format!("touch {}", marker.display()));

        session.close_terminal();
        // Give the shell every chance to act on its input before checking.
        session.wait_for_proxy_exit(EXIT_DEADLINE);
        drop(session);

        assert!(
            !marker.exists(),
            "attempt {attempt}: closing the terminal executed the half-typed command"
        );
    }
}
