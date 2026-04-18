//! SSH config host parser with fingerprint-based caching.
//!
//! Reads `~/.ssh/config` and extracts `Host` directive values, skipping
//! wildcard entries (`*`). Re-parses when the file's `(mtime, len)`
//! fingerprint changes — pairing size with mtime catches rapid edits that
//! land on the same mtime second but produce different content.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// Composite freshness key. Pairing mtime with length catches the rare but
/// real case where two successive writes land on the same mtime second
/// (1s resolution filesystems) but change content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileFingerprint {
    mtime: SystemTime,
    len: u64,
}

impl FileFingerprint {
    fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        Some(Self {
            mtime,
            len: meta.len(),
        })
    }
}

/// Cached SSH host list with fingerprint tracking.
pub struct SshHostCache {
    state: Mutex<SshHostState>,
    path: PathBuf,
}

struct SshHostState {
    hosts: Vec<String>,
    fingerprint: Option<FileFingerprint>,
}

impl SshHostCache {
    /// Create a new cache pointing at the given SSH config path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            state: Mutex::new(SshHostState {
                hosts: Vec::new(),
                fingerprint: None,
            }),
            path,
        }
    }

    /// Create a cache pointing at `~/.ssh/config`.
    pub fn default_path() -> Option<Self> {
        dirs::home_dir().map(|h| Self::new(h.join(".ssh").join("config")))
    }

    /// Return the cached hosts, refreshing from disk if the file's mtime
    /// has changed since the last read.
    pub fn hosts(&self) -> Vec<String> {
        self.refresh_if_stale();
        match self.state.lock() {
            Ok(state) => state.hosts.clone(),
            Err(e) => {
                tracing::debug!("SSH host cache lock poisoned: {e}");
                Vec::new()
            }
        }
    }

    /// Return only hosts matching a prefix, avoiding cloning the entire list
    /// on every keystroke. Empty prefix returns all hosts.
    pub fn hosts_matching(&self, prefix: &str) -> Vec<String> {
        self.refresh_if_stale();
        match self.state.lock() {
            Ok(state) => {
                if prefix.is_empty() {
                    state.hosts.clone()
                } else {
                    state
                        .hosts
                        .iter()
                        .filter(|h| h.starts_with(prefix))
                        .cloned()
                        .collect()
                }
            }
            Err(e) => {
                tracing::debug!("SSH host cache lock poisoned: {e}");
                Vec::new()
            }
        }
    }

    fn refresh_if_stale(&self) {
        let current_fp = match FileFingerprint::from_path(&self.path) {
            Some(fp) => fp,
            None => return, // file missing or unreadable — keep existing
        };

        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("SSH host cache lock poisoned in refresh: {e}");
                return;
            }
        };
        if state.fingerprint == Some(current_fp) {
            return; // unchanged
        }

        match std::fs::read_to_string(&self.path) {
            Ok(contents) => {
                state.hosts = parse_ssh_hosts_from_str(&contents);
                state.fingerprint = Some(current_fp);
            }
            Err(e) => {
                tracing::debug!("failed to read SSH config: {e}");
                state.fingerprint = Some(current_fp);
            }
        }
    }
}

/// Parse SSH `Host` directives from a config file at the given path.
///
/// - Skips wildcard entries containing `*` or `?`.
/// - Handles multiple hosts on one line (`Host foo bar`).
/// - `Include` directives are ignored (no recursion).
/// - The `Host` keyword is matched case-insensitively.
pub fn parse_ssh_hosts(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => parse_ssh_hosts_from_str(&contents),
        Err(e) => {
            tracing::debug!("failed to read SSH config at {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Parse SSH hosts from already-read config contents.
fn parse_ssh_hosts_from_str(contents: &str) -> Vec<String> {
    let mut hosts = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();

        // Skip comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Match "Host" keyword case-insensitively
        let lower = trimmed.to_ascii_lowercase();
        if !lower.starts_with("host") {
            continue;
        }

        // Must be exactly "host" followed by whitespace (not "hostname")
        let after_host = &trimmed[4..];
        if after_host.is_empty() || !after_host.starts_with(char::is_whitespace) {
            continue;
        }

        // Extract host patterns after the keyword
        for pattern in after_host.split_whitespace() {
            // Skip wildcards
            if pattern.contains('*') || pattern.contains('?') {
                continue;
            }
            if !pattern.is_empty() {
                hosts.push(pattern.to_string());
            }
        }
    }

    hosts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_basic_host_parsing() {
        let config = "\
Host foo
    HostName foo.example.com
    User admin

Host bar
    HostName bar.example.com
";
        let hosts = parse_ssh_hosts_from_str(config);
        assert_eq!(hosts, vec!["foo", "bar"]);
    }

    #[test]
    fn test_multiple_hosts_on_one_line() {
        let config = "Host foo bar baz\n    User admin\n";
        let hosts = parse_ssh_hosts_from_str(config);
        assert_eq!(hosts, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn test_wildcard_skipped() {
        let config = "\
Host *
    ServerAliveInterval 60

Host prod
    HostName prod.example.com

Host *.internal
    User deploy
";
        let hosts = parse_ssh_hosts_from_str(config);
        assert_eq!(hosts, vec!["prod"]);
    }

    #[test]
    fn test_empty_config() {
        let hosts = parse_ssh_hosts_from_str("");
        assert!(hosts.is_empty());
    }

    #[test]
    fn test_missing_file_returns_empty() {
        let hosts = parse_ssh_hosts(Path::new("/nonexistent/path/ssh_config"));
        assert!(hosts.is_empty());
    }

    #[test]
    fn test_case_insensitive_host_keyword() {
        let config = "\
host lowercase-host
    HostName lc.example.com

HOST uppercase-host
    HostName uc.example.com

Host mixed-host
    HostName mx.example.com
";
        let hosts = parse_ssh_hosts_from_str(config);
        assert_eq!(
            hosts,
            vec!["lowercase-host", "uppercase-host", "mixed-host"]
        );
    }

    #[test]
    fn test_comments_and_blank_lines_ignored() {
        let config = "\
# This is a comment
Host alpha

   # Another comment

Host beta
";
        let hosts = parse_ssh_hosts_from_str(config);
        assert_eq!(hosts, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_hostname_not_confused_with_host() {
        let config = "\
Host myserver
    HostName myserver.example.com
    Port 22
";
        let hosts = parse_ssh_hosts_from_str(config);
        assert_eq!(hosts, vec!["myserver"]);
    }

    #[test]
    fn test_include_directive_ignored() {
        let config = "\
Include ~/.ssh/config.d/*

Host from-main
    HostName main.example.com
";
        let hosts = parse_ssh_hosts_from_str(config);
        assert_eq!(hosts, vec!["from-main"]);
    }

    #[test]
    fn test_question_mark_wildcard_skipped() {
        let config = "\
Host prod-?
    User deploy

Host staging
    HostName staging.example.com
";
        let hosts = parse_ssh_hosts_from_str(config);
        assert_eq!(hosts, vec!["staging"]);
    }

    #[test]
    fn test_cache_loads_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config");
        std::fs::write(&config_path, "Host alpha\nHost beta\n").unwrap();

        let cache = SshHostCache::new(config_path);
        let hosts = cache.hosts();
        assert_eq!(hosts, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_cache_refreshes_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config");
        std::fs::write(&config_path, "Host alpha\n").unwrap();

        let cache = SshHostCache::new(config_path.clone());
        assert_eq!(cache.hosts(), vec!["alpha"]);

        // Append a new host
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&config_path)
                .unwrap();
            writeln!(f, "Host beta").unwrap();
        }
        // Bump mtime so the cache detects a change
        let future = SystemTime::now() + std::time::Duration::from_secs(2);
        filetime::set_file_mtime(&config_path, filetime::FileTime::from_system_time(future))
            .unwrap();

        let hosts = cache.hosts();
        assert_eq!(hosts, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_cache_missing_file_returns_empty() {
        let cache = SshHostCache::new(PathBuf::from("/nonexistent/ssh/config"));
        assert!(cache.hosts().is_empty());
    }
}
