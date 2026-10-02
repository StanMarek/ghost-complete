//! Mirror the shell's working directory onto the proxy process.
//!
//! The proxy is the direct child of whatever spawned the pane, so tools that
//! seed new panes from that child's cwd — Zellij `NewPane`/`NewTab`, tmux
//! `#{pane_current_path}` — read *our* cwd, not the inner shell's (#172).
//! Following each local OSC 7 report with a real `chdir` keeps them in sync.
//!
//! Anything the proxy reads relative to its own cwd after startup must be
//! pinned first. Filesystem spec dirs are made absolute when they are
//! registered, because spec files are read lazily. Completion work uses the
//! parser's cwd. It falls back to `.` only before the first report, when the
//! proxy has not moved yet.

use std::path::Path;

pub(crate) struct ProcessCwdSync {
    /// Hostname when the proxy started. The inner shell snapshots `$HOST` at
    /// startup too, so this keeps matching its reports after macOS renames
    /// the machine on a network change.
    startup_hostname: Option<String>,
}

impl ProcessCwdSync {
    pub(crate) fn new() -> Self {
        Self {
            startup_hostname: local_hostname(),
        }
    }

    /// `chdir` to `path` if `host` names this machine. A remote report (shell
    /// inside `ssh`) is ignored, so a pane opened mid-session lands in the
    /// local shell's directory and not in a same-named local one. Integrations
    /// installed before the per-prompt zsh report also never move it back.
    ///
    /// Failures are logged and leave the previous cwd in place — completions
    /// track the shell cwd separately, so this only affects what the pane's
    /// parent inherits.
    pub(crate) fn follow(&self, host: &str, path: &Path) {
        if !self.is_local(host, local_hostname) {
            tracing::debug!(%host, ?path, "OSC 7 from non-local host — proxy cwd unchanged");
            return;
        }
        if let Err(e) = std::env::set_current_dir(path) {
            tracing::debug!(?path, "failed to follow shell cwd: {e}");
        }
    }

    /// `current_hostname` is only consulted when the cheap checks miss, which
    /// keeps the `gethostname` syscall off the common path.
    fn is_local(&self, host: &str, current_hostname: impl FnOnce() -> Option<String>) -> bool {
        host.is_empty()
            || host.eq_ignore_ascii_case("localhost")
            || self
                .startup_hostname
                .as_deref()
                .is_some_and(|name| host.eq_ignore_ascii_case(name))
            || current_hostname().is_some_and(|name| host.eq_ignore_ascii_case(&name))
    }
}

fn local_hostname() -> Option<String> {
    // 256 covers macOS MAXHOSTNAMELEN and Linux HOST_NAME_MAX (64).
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for writes of `buf.len()` bytes.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    // POSIX leaves NUL termination unspecified on truncation.
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync_started_on(hostname: &str) -> ProcessCwdSync {
        ProcessCwdSync {
            startup_hostname: Some(hostname.to_string()),
        }
    }

    fn no_current_hostname() -> Option<String> {
        None
    }

    #[test]
    fn empty_host_is_local() {
        // RFC 8089: `file:///path` has an empty authority meaning "this host".
        assert!(sync_started_on("mac").is_local("", no_current_hostname));
    }

    #[test]
    fn localhost_is_local_case_insensitive() {
        let sync = sync_started_on("mac");
        assert!(sync.is_local("localhost", no_current_hostname));
        assert!(sync.is_local("LocalHost", no_current_hostname));
    }

    #[test]
    fn startup_hostname_is_local_case_insensitive() {
        let sync = sync_started_on("Stans-MacBook.local");
        assert!(sync.is_local("Stans-MacBook.local", no_current_hostname));
        assert!(sync.is_local("stans-macbook.local", no_current_hostname));
    }

    #[test]
    fn current_hostname_is_local_after_rename() {
        // A shell started after the rename reports the new name.
        let sync = sync_started_on("old-name");
        assert!(sync.is_local("new-name", || Some("new-name".to_string())));
    }

    #[test]
    fn foreign_host_is_not_local() {
        let sync = sync_started_on("mac");
        assert!(!sync.is_local("build-box", || Some("mac".to_string())));
    }

    #[test]
    fn unknown_hostname_only_accepts_unambiguous_hosts() {
        let sync = ProcessCwdSync {
            startup_hostname: None,
        };
        assert!(sync.is_local("", no_current_hostname));
        assert!(!sync.is_local("mac", no_current_hostname));
    }

    #[test]
    fn local_hostname_is_readable() {
        let name = local_hostname().expect("gethostname should succeed");
        assert!(!name.is_empty());
        assert!(!name.contains('\0'));
    }
}
