use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// A ghost-complete process running inside a PTY for integration testing.
///
/// Creates a PTY-in-PTY architecture: test PTY → ghost-complete → inner PTY → /bin/sh.
pub struct GhostProcess {
    writer: Box<dyn Write + Send>,
    output: Arc<(Mutex<Vec<u8>>, Condvar)>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    pid: Option<u32>,
}

impl GhostProcess {
    /// Spawn ghost-complete inside a PTY wrapping /bin/sh.
    pub fn spawn() -> Self {
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
        cmd.args(["--log-level", "error", "/bin/sh"]);

        let child = pty_pair
            .slave
            .spawn_command(cmd)
            .expect("failed to spawn ghost-complete");

        let pid = child.process_id();

        let writer = pty_pair
            .master
            .take_writer()
            .expect("failed to take PTY writer");
        let mut reader = pty_pair
            .master
            .try_clone_reader()
            .expect("failed to clone PTY reader");

        // Shared output buffer with condvar for blocking reads.
        let output = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let output_clone = Arc::clone(&output);

        // Background reader thread: accumulates PTY output.
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

        // Wait for shell to initialize (larger binary with embedded specs needs more time).
        thread::sleep(Duration::from_millis(1500));

        GhostProcess {
            writer,
            output,
            child,
            pid,
        }
    }

    /// Send a line to the PTY (appends \r for "Enter").
    pub fn send_line(&mut self, line: &str) {
        let data = format!("{}\r", line);
        self.writer
            .write_all(data.as_bytes())
            .expect("failed to write to PTY");
        self.writer.flush().expect("failed to flush PTY writer");
    }

    /// Write raw bytes to the PTY.
    #[allow(dead_code)]
    pub fn write_raw(&mut self, data: &[u8]) {
        self.writer
            .write_all(data)
            .expect("failed to write raw to PTY");
        self.writer.flush().expect("failed to flush PTY writer");
    }

    /// Block until `substr` appears in the accumulated output, or timeout after 10s.
    pub fn expect_output(&self, substr: &str) {
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

    /// Return a snapshot of all accumulated output.
    pub fn output_snapshot(&self) -> Vec<u8> {
        let (lock, _) = &*self.output;
        lock.lock().unwrap().clone()
    }

    /// Return the current length of the accumulated output. Used with
    /// `wait_for_bytes_after` to scan only bytes produced after a mark.
    #[allow(dead_code)]
    pub fn output_len(&self) -> usize {
        let (lock, _) = &*self.output;
        lock.lock().unwrap().len()
    }

    /// Block until `needle` bytes appear in the output at or after
    /// `start_offset`, or `timeout` elapses. Returns `true` on match, `false`
    /// on timeout — never panics.
    ///
    /// Unlike `expect_output`, this works on raw bytes (so ANSI escape
    /// markers like `\x1b7` can be matched) and does not panic on timeout
    /// so callers can build non-fatal readiness probes.
    #[allow(dead_code)]
    pub fn wait_for_bytes_after(
        &self,
        needle: &[u8],
        start_offset: usize,
        timeout: Duration,
    ) -> bool {
        let start = Instant::now();
        let (lock, cvar) = &*self.output;
        loop {
            let data = lock.lock().unwrap();
            if data.len() > start_offset && contains_subslice(&data[start_offset..], needle) {
                return true;
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return false;
            }
            let remaining = timeout - elapsed;
            let (data, _) = cvar.wait_timeout(data, remaining).unwrap();
            if data.len() > start_offset && contains_subslice(&data[start_offset..], needle) {
                return true;
            }
        }
    }

    /// Send `exit <code>` and wait for the process to exit. Returns the exit code.
    pub fn exit_with_code(&mut self, code: i32) -> i32 {
        self.send_line(&format!("exit {}", code));
        self.wait_for_exit()
    }

    /// Return the PID of the ghost-complete process (if available).
    #[allow(dead_code)]
    pub fn child_pid(&self) -> Option<u32> {
        self.pid
    }

    /// Wait for the child process to exit, polling every 50ms. Kills after 15s.
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

impl Drop for GhostProcess {
    fn drop(&mut self) {
        // Kill the process if it's still running.
        if self.child.try_wait().ok().flatten().is_none() {
            self.child.kill().ok();
        }
    }
}

/// Byte-level substring search — avoids `String::from_utf8_lossy`
/// allocations on hot polling paths when watching raw ANSI output.
#[allow(dead_code)]
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}
