use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::provider::Provider;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

pub(crate) const DEFAULT_MAX_HISTORY_ENTRIES: usize = 10_000;

/// Above this file size, [`read_tail`] reads only the last ~2 MiB rather
/// than slurping the whole file. 2 MiB is roughly 20–30k history entries on
/// typical zsh lines, comfortably above `DEFAULT_MAX_HISTORY_ENTRIES`, so
/// tail-reading never drops an entry the caller would have kept.
const TAIL_THRESHOLD_BYTES: u64 = 2 * 1024 * 1024;
const TAIL_READ_BYTES: u64 = 2 * 1024 * 1024;

pub struct HistoryProvider {
    state: Mutex<HistoryState>,
    /// `None` for test/bench constructors — never refreshes.
    path: Option<PathBuf>,
    max_entries: usize,
}

/// Composite freshness key. Pairing mtime with length catches rapid writes
/// that land on the same mtime second (common on filesystems with 1s mtime
/// resolution) but produce different content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileFingerprint {
    mtime: SystemTime,
    len: u64,
}

impl FileFingerprint {
    fn from_metadata(meta: &std::fs::Metadata) -> Option<Self> {
        let mtime = meta.modified().ok()?;
        Some(Self {
            mtime,
            len: meta.len(),
        })
    }

    fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Self::from_metadata(&meta)
    }
}

struct HistoryState {
    entries: Vec<String>,
    fingerprint: Option<FileFingerprint>,
}

impl HistoryProvider {
    pub fn load(max_entries: usize) -> Self {
        let path = Self::history_path().ok();
        let (entries, fingerprint) = match &path {
            Some(p) => {
                let fp = FileFingerprint::from_path(p);
                match Self::read_history_from(p, max_entries) {
                    Ok(entries) => (entries, fp),
                    Err(e) => {
                        tracing::debug!("failed to load history: {e}");
                        (Vec::new(), None)
                    }
                }
            }
            None => {
                tracing::debug!("failed to load history: could not determine history file path");
                (Vec::new(), None)
            }
        };
        Self {
            state: Mutex::new(HistoryState {
                entries,
                fingerprint,
            }),
            path,
            max_entries,
        }
    }

    /// Test/bench constructor — inject entries directly. Never refreshes.
    pub fn from_entries(entries: Vec<String>) -> Self {
        Self {
            state: Mutex::new(HistoryState {
                entries,
                fingerprint: None,
            }),
            path: None,
            max_entries: 0,
        }
    }

    /// Re-read the history file if its `(mtime, len)` fingerprint has
    /// changed. Pairing with length catches rapid edits that land on the
    /// same mtime second but change content. Does nothing if the provider
    /// was created via `from_entries()`.
    fn refresh_if_stale(&self) {
        let path = match &self.path {
            Some(p) => p,
            None => return,
        };

        let current_fp = match FileFingerprint::from_path(path) {
            Some(fp) => fp,
            None => return, // can't stat — keep existing entries
        };

        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("history lock poisoned in refresh: {e}");
                return;
            }
        };
        if state.fingerprint == Some(current_fp) {
            return; // unchanged
        }

        match Self::read_history_from(path, self.max_entries) {
            Ok(entries) => {
                state.entries = entries;
                state.fingerprint = Some(current_fp);
            }
            Err(e) => {
                tracing::debug!("failed to refresh history: {e}");
                // keep existing entries, but update fingerprint so we
                // don't retry every call
                state.fingerprint = Some(current_fp);
            }
        }
    }

    fn read_history_from(path: &Path, max_entries: usize) -> Result<Vec<String>> {
        let raw = read_tail(path)?;
        // Strict per-line UTF-8: any line that isn't valid UTF-8 is dropped
        // with a debug log instead of being silently corrupted by U+FFFD
        // replacement characters (which would then end up rendered in the
        // popup and selected back into the user's command line). Splitting
        // by `\n` first means a single bad line doesn't poison the rest of
        // the file.
        let mut clean = String::with_capacity(raw.len());
        for line in raw.split(|b| *b == b'\n') {
            match std::str::from_utf8(line) {
                Ok(s) => {
                    clean.push_str(s);
                    clean.push('\n');
                }
                Err(e) => {
                    tracing::debug!("skipping non-UTF-8 history line in {path:?}: {e}");
                }
            }
        }
        Ok(Self::parse_and_dedup(&clean, max_entries))
    }

    /// Resolve the history file path from `$HISTFILE` (falling back to
    /// `~/.zsh_history`) and validate it.
    ///
    /// `$HISTFILE` is read from the environment, which a malicious dotfile
    /// (e.g. a compromised `.zshenv`) can set to anything — `/etc/passwd`,
    /// `~/.ssh/id_rsa`, `~/.aws/credentials`, etc. Without validation the
    /// proxy would happily slurp the file, parse it as zsh history, and
    /// render the contents in the popup. That's a local info-disclosure.
    ///
    /// Validation rules (must all pass; on failure we log a `warn!` and
    /// fall back to `~/.zsh_history`, which itself must validate):
    ///
    /// 1. The resolved path (after canonicalizing through symlinks) must
    ///    live under the canonicalized `$HOME`. If the file doesn't exist
    ///    yet (fresh install), we canonicalize the parent directory and
    ///    re-attach the filename — we still need the parent to be inside
    ///    `$HOME` so an attacker can't point at a symlink chain that
    ///    eventually escapes.
    /// 2. The filename must look like a shell history file:
    ///    - exact match: `.zsh_history`, `.bash_history`, `.fish_history`,
    ///      `.histfile`, `history`, `.history`
    ///    - ends in `_history`, `.history`, or `.hist`
    ///
    ///   Targets like `/etc/passwd` or `~/.ssh/id_rsa` are rejected by
    ///   rule 1 or 2 (or both).
    fn history_path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;

        if let Ok(histfile) = std::env::var("HISTFILE") {
            let candidate = PathBuf::from(&histfile);
            match Self::validate_history_path(&candidate, &home) {
                Ok(validated) => return Ok(validated),
                Err(e) => {
                    tracing::warn!(
                        "ignoring $HISTFILE={histfile:?}: {e}; falling back to ~/.zsh_history"
                    );
                }
            }
        }

        let default = home.join(".zsh_history");
        // Default path must also validate (canonicalization could still
        // escape if $HOME itself is a symlink chain into a weird place,
        // and the filename rule is already satisfied by ".zsh_history").
        Self::validate_history_path(&default, &home)
    }

    /// Validate that `path` is safe to read as a shell history file.
    /// Returns the (possibly canonicalized) path on success.
    fn validate_history_path(path: &Path, home: &Path) -> Result<PathBuf> {
        // Filename check first — cheap and rejects the most obvious abuse
        // (`HISTFILE=/etc/passwd`, `HISTFILE=~/.ssh/id_rsa`) before we
        // touch the filesystem.
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("path has no filename component"))?;

        if !is_history_filename(file_name) {
            anyhow::bail!("filename {file_name:?} does not look like a shell history file");
        }

        // Canonicalize $HOME once so the prefix check uses the resolved
        // path (matters on macOS where /tmp -> /private/tmp etc.).
        let home_canon = std::fs::canonicalize(home)
            .map_err(|e| anyhow::anyhow!("could not canonicalize $HOME: {e}"))?;

        // Resolve the candidate. If the file exists, canonicalize it.
        // If it doesn't (fresh install — no history yet), canonicalize the
        // parent so we still detect a symlinked-out parent directory.
        let canonical = match std::fs::canonicalize(path) {
            Ok(p) => p,
            Err(_) => {
                let parent = path
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("path has no parent directory"))?;
                let parent_canon = std::fs::canonicalize(parent).map_err(|e| {
                    anyhow::anyhow!("could not canonicalize parent {parent:?}: {e}")
                })?;
                parent_canon.join(file_name)
            }
        };

        if !canonical.starts_with(&home_canon) {
            anyhow::bail!("resolved path {canonical:?} is outside $HOME ({home_canon:?})");
        }

        Ok(canonical)
    }

    fn parse_and_dedup(contents: &str, max_entries: usize) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut entries = Vec::new();

        // Merge multi-line commands (zsh writes an unescaped trailing `\`
        // followed by a newline when a command continues onto the next
        // physical line) before dedup so fragments don't surface as their
        // own entries.
        let merged = merge_multiline_entries(contents);

        // Process lines in reverse so we keep the most recent occurrence
        for line in merged.iter().rev() {
            let cmd = parse_history_line(line);
            if cmd.is_empty() {
                continue;
            }
            if seen.insert(cmd.to_string()) {
                entries.push(cmd.to_string());
            }
            if entries.len() >= max_entries {
                break;
            }
        }

        // Reverse back so most recent is last (but deduped)
        entries.reverse();
        entries
    }
}

/// Read the tail of a file up to `TAIL_READ_BYTES`. For small files, read
/// the whole file. For large files, seek to `len - TAIL_READ_BYTES` then
/// advance past the first `\n` so the truncated head is never a mid-line
/// split (which would otherwise either corrupt UTF-8 validation or emit a
/// fragment into the history suggestions).
fn read_tail(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();

    if len <= TAIL_THRESHOLD_BYTES {
        let mut buf = Vec::with_capacity(len as usize);
        file.read_to_end(&mut buf)?;
        return Ok(buf);
    }

    let start = len.saturating_sub(TAIL_READ_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(TAIL_READ_BYTES as usize + 256);
    file.read_to_end(&mut buf)?;

    // Advance past the first partial line so we never emit a mid-line
    // fragment. If the tail happens to begin on a line boundary, the
    // first `\n` is at index 0 and we simply drop that empty prefix.
    if let Some(nl) = buf.iter().position(|b| *b == b'\n') {
        buf.drain(..=nl);
    }
    Ok(buf)
}

/// Returns true if `name` looks like a conventional shell history filename.
///
/// Matched by exact name (covers `.zsh_history`, `.bash_history`,
/// `.fish_history`, `.histfile`, `history`, `.history`) or by suffix
/// (`_history`, `.history`, `.hist` — covers `something_history`,
/// `mytool.hist`, etc.). Anything else (`passwd`, `id_rsa`, `credentials`,
/// `config`, `.env`) is rejected.
fn is_history_filename(name: &str) -> bool {
    const EXACT: &[&str] = &[
        ".zsh_history",
        ".bash_history",
        ".fish_history",
        ".histfile",
        "history",
        ".history",
    ];
    const SUFFIXES: &[&str] = &["_history", ".history", ".hist"];

    if EXACT.contains(&name) {
        return true;
    }
    SUFFIXES
        .iter()
        .any(|s| name.len() > s.len() && name.ends_with(s))
}

/// Merge zsh multi-line history entries. A physical line ending in an
/// unescaped trailing backslash continues onto the next physical line.
/// An "unescaped" backslash is one preceded by an even number of
/// backslashes — e.g. `foo \` continues, but `foo \\` does not.
fn merge_multiline_entries(contents: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;

    for line in contents.lines() {
        let continues = has_unescaped_trailing_backslash(line);
        let body = if continues {
            // Strip the single trailing `\` that marks the continuation.
            &line[..line.len() - 1]
        } else {
            line
        };

        match pending.as_mut() {
            Some(acc) => {
                acc.push('\n');
                acc.push_str(body);
            }
            None => {
                pending = Some(body.to_string());
            }
        }

        if !continues {
            if let Some(full) = pending.take() {
                out.push(full);
            }
        }
    }

    // If the file ends mid-continuation (no final newline after the last
    // `\`), still emit what we accumulated so the user sees the partial.
    if let Some(full) = pending.take() {
        out.push(full);
    }
    out
}

fn has_unescaped_trailing_backslash(line: &str) -> bool {
    if !line.ends_with('\\') {
        return false;
    }
    // Count trailing backslashes; an odd count means the final one is
    // unescaped (and therefore a continuation marker).
    let mut count = 0usize;
    for b in line.bytes().rev() {
        if b == b'\\' {
            count += 1;
        } else {
            break;
        }
    }
    count % 2 == 1
}

/// Parse a single history line, handling both zsh extended format and plain.
///
/// Zsh extended format: `: 1234567890:0;command here`
/// Plain format: `command here`
fn parse_history_line(line: &str) -> &str {
    let trimmed = line.trim();
    if trimmed.starts_with(": ") {
        // Zsh extended format — find the semicolon after the timestamp
        if let Some(idx) = trimmed.find(';') {
            return trimmed[idx + 1..].trim();
        }
    }
    trimmed
}

impl Provider for HistoryProvider {
    fn provide(&self, ctx: &CommandContext, _cwd: &Path) -> Result<Vec<Suggestion>> {
        // History only makes sense in the first segment — not after |, &&, ||, or ;
        if !ctx.is_first_segment {
            return Ok(Vec::new());
        }

        self.refresh_if_stale();

        let state = match self.state.lock() {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("history lock poisoned in provide: {e}");
                return Ok(Vec::new());
            }
        };
        let suggestions = state
            .entries
            .iter()
            .map(|entry| Suggestion {
                text: entry.clone(),
                description: None,
                kind: SuggestionKind::History,
                source: SuggestionSource::History,
                ..Default::default()
            })
            .collect();

        Ok(suggestions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_buffer::QuoteState;

    fn cmd_position_ctx(word: &str) -> CommandContext {
        CommandContext {
            command: None,
            args: vec![],
            current_word: word.to_string(),
            word_index: 0,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        }
    }

    #[test]
    fn test_parse_extended_history() {
        let line = ": 1234567890:0;git push";
        assert_eq!(parse_history_line(line), "git push");
    }

    #[test]
    fn test_parse_plain_history() {
        let line = "cargo build --release";
        assert_eq!(parse_history_line(line), "cargo build --release");
    }

    #[test]
    fn test_history_suppressed_in_pipe() {
        let provider = HistoryProvider::from_entries(vec!["git push".into(), "ls -la".into()]);
        let mut ctx = cmd_position_ctx("");
        ctx.in_pipe = true;
        ctx.is_first_segment = false;
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.is_empty(),
            "history should be empty in pipe segment"
        );
    }

    #[test]
    fn test_history_returns_full_commands() {
        let provider = HistoryProvider::from_entries(vec!["git push".into(), "ls -la".into()]);
        let ctx = cmd_position_ctx("gi");
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|s| s.text == "git push"));
        assert!(results.iter().any(|s| s.text == "ls -la"));
        assert!(results.iter().all(|s| s.description.is_none()));
    }

    #[test]
    fn test_history_available_at_arg_position_in_first_segment() {
        let provider = HistoryProvider::from_entries(vec!["git push origin main".into()]);
        let mut ctx = cmd_position_ctx("");
        ctx.command = Some("git".into());
        ctx.word_index = 1;
        ctx.is_first_segment = true;
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "git push origin main");
    }

    #[test]
    fn test_from_entries_does_not_refresh() {
        // from_entries sets path to None, so refresh_if_stale is a no-op.
        let provider = HistoryProvider::from_entries(vec!["echo hello".into()]);
        assert!(provider.path.is_none());
        assert_eq!(provider.max_entries, 0);

        // Calling provide (which calls refresh_if_stale internally) should
        // still return the injected entries without error.
        let ctx = cmd_position_ctx("");
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "echo hello");
    }

    #[test]
    fn test_refresh_picks_up_new_entries() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let hist_path = dir.path().join("test_history");

        // Write initial history file.
        std::fs::write(&hist_path, "ls\ncd /tmp\n").unwrap();

        // Build provider pointing at the temp file.
        let provider = HistoryProvider {
            state: Mutex::new(HistoryState {
                entries: HistoryProvider::parse_and_dedup("ls\ncd /tmp\n", 1000),
                fingerprint: FileFingerprint::from_path(&hist_path),
            }),
            path: Some(hist_path.clone()),
            max_entries: 1000,
        };

        let ctx = cmd_position_ctx("");
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert_eq!(results.len(), 2);

        // Append a new command. We must ensure the mtime actually changes;
        // on some filesystems the resolution is 1 second, so bump it explicitly.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&hist_path)
                .unwrap();
            writeln!(f, "git status").unwrap();
        }
        // Force mtime forward so the provider sees a change.
        let future = SystemTime::now() + std::time::Duration::from_secs(2);
        filetime::set_file_mtime(&hist_path, filetime::FileTime::from_system_time(future)).unwrap();

        // provide() should pick up the new entry via refresh_if_stale.
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert_eq!(results.len(), 3);
        assert!(results.iter().any(|s| s.text == "git status"));
    }

    #[test]
    fn test_is_history_filename_accepts_known_names() {
        assert!(is_history_filename(".zsh_history"));
        assert!(is_history_filename(".bash_history"));
        assert!(is_history_filename(".fish_history"));
        assert!(is_history_filename(".histfile"));
        assert!(is_history_filename("history"));
        assert!(is_history_filename(".history"));
        // Suffix matches.
        assert!(is_history_filename("custom_history"));
        assert!(is_history_filename("mytool.history"));
        assert!(is_history_filename("repl.hist"));
    }

    #[test]
    fn test_is_history_filename_rejects_sensitive_names() {
        assert!(!is_history_filename("passwd"));
        assert!(!is_history_filename("id_rsa"));
        assert!(!is_history_filename("credentials"));
        assert!(!is_history_filename("config"));
        assert!(!is_history_filename(".env"));
        assert!(!is_history_filename("authorized_keys"));
        // Suffix-only must have content before the suffix.
        assert!(!is_history_filename("_history"));
        assert!(!is_history_filename(".hist"));
    }

    #[test]
    fn test_validate_rejects_etc_passwd() {
        // Simulate an attacker dotfile setting HISTFILE=/etc/passwd.
        // The fake $HOME is a tempdir; /etc/passwd is obviously not under it,
        // but the filename check fires first and rejects it regardless.
        let home = tempfile::tempdir().unwrap();
        let result = HistoryProvider::validate_history_path(Path::new("/etc/passwd"), home.path());
        assert!(result.is_err(), "must reject /etc/passwd");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not look like a shell history file"),
            "expected filename-rejection error, got: {err}"
        );
    }

    #[test]
    fn test_validate_rejects_ssh_key_under_home() {
        // HISTFILE=~/.ssh/id_rsa — under $HOME but the filename is wrong.
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
        let key = home.path().join(".ssh/id_rsa");
        std::fs::write(&key, b"PRIVATE KEY").unwrap();
        let result = HistoryProvider::validate_history_path(&key, home.path());
        assert!(result.is_err(), "must reject ~/.ssh/id_rsa");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not look like a shell history file"),
            "expected filename-rejection error, got: {err}"
        );
    }

    #[test]
    fn test_validate_accepts_zsh_history_under_home() {
        let home = tempfile::tempdir().unwrap();
        let hist = home.path().join(".zsh_history");
        std::fs::write(&hist, "ls\n").unwrap();
        let result = HistoryProvider::validate_history_path(&hist, home.path());
        assert!(result.is_ok(), "expected accept, got: {result:?}");
    }

    #[test]
    fn test_validate_accepts_nonexistent_zsh_history() {
        // Fresh install: file doesn't exist yet but parent does.
        let home = tempfile::tempdir().unwrap();
        let hist = home.path().join(".zsh_history");
        let result = HistoryProvider::validate_history_path(&hist, home.path());
        assert!(
            result.is_ok(),
            "must accept nonexistent file under valid parent: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_rejects_symlink_escaping_home() {
        // Build a fake $HOME, then put a symlink inside it pointing at
        // /etc/passwd. Even though the symlink path itself lives under
        // $HOME, canonicalization must follow it and reject the result.
        // The filename of the symlink itself is a valid history name, so
        // the filename check passes — only the canonical-path check
        // rejects it. This is the load-bearing test for the symlink rule.
        let home = tempfile::tempdir().unwrap();
        let link = home.path().join(".zsh_history");
        std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
        let result = HistoryProvider::validate_history_path(&link, home.path());
        assert!(
            result.is_err(),
            "must reject symlink that escapes $HOME, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("outside $HOME"),
            "expected outside-$HOME error, got: {err}"
        );
    }

    #[test]
    fn test_read_history_skips_invalid_utf8_lines() {
        // Mix of valid UTF-8 lines and one line with invalid bytes.
        // The invalid line must be dropped; the valid ones survive.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom_history");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"good command one\n");
        bytes.extend_from_slice(b"bad \xFF\xFE bytes here\n");
        bytes.extend_from_slice(b"good command two\n");
        std::fs::write(&path, &bytes).unwrap();

        let entries = HistoryProvider::read_history_from(&path, 1000).unwrap();
        // Order: parse_and_dedup reverses to most-recent-last, so
        // "good command two" should be last.
        assert_eq!(entries.len(), 2, "got entries: {entries:?}");
        assert!(entries.iter().any(|e| e == "good command one"));
        assert!(entries.iter().any(|e| e == "good command two"));
        assert!(
            !entries.iter().any(|e| e.contains('\u{FFFD}')),
            "must not emit replacement chars"
        );
    }

    #[test]
    fn test_read_tail_small_file_full_read() {
        // File under the threshold: read_tail returns the full contents.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small_history");
        let body = b"echo one\necho two\n";
        std::fs::write(&path, body).unwrap();

        let got = read_tail(&path).unwrap();
        assert_eq!(got, body);
    }

    #[test]
    fn test_read_tail_large_file_returns_tail_only() {
        // Build a file just over TAIL_THRESHOLD_BYTES. Tail must not
        // exceed TAIL_READ_BYTES meaningfully and must not start mid-line.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big_history");

        let mut f = std::fs::File::create(&path).unwrap();
        use std::io::Write;
        let mut i: u64 = 0;
        let target = 3 * 1024 * 1024_u64;
        let mut written: u64 = 0;
        while written < target {
            let line = format!("line {i:08}\n");
            f.write_all(line.as_bytes()).unwrap();
            written += line.len() as u64;
            i += 1;
        }
        f.sync_all().unwrap();
        drop(f);

        let got = read_tail(&path).unwrap();
        assert!(
            (got.len() as u64) <= TAIL_READ_BYTES + 64,
            "tail length {} must not meaningfully exceed TAIL_READ_BYTES",
            got.len()
        );
        assert!(
            got.starts_with(b"line "),
            "tail must begin on a line boundary, got prefix {:?}",
            &got[..20.min(got.len())]
        );
    }

    #[test]
    fn test_merge_multiline_basic_continuation() {
        // A command broken across two lines should emerge as one entry.
        let contents = "first line\nfoo \\\nbar\nthird line\n";
        let merged = merge_multiline_entries(contents);
        assert_eq!(
            merged,
            vec![
                "first line".to_string(),
                "foo \nbar".to_string(),
                "third line".to_string(),
            ]
        );
    }

    #[test]
    fn test_merge_multiline_escaped_trailing_backslash_does_not_continue() {
        // `echo foo\\` is an escaped backslash — not a continuation.
        let contents = "echo foo\\\\\nnext\n";
        let merged = merge_multiline_entries(contents);
        assert_eq!(merged, vec!["echo foo\\\\".to_string(), "next".to_string()]);
    }

    #[test]
    fn test_merge_multiline_three_line_continuation() {
        let contents = "a \\\nb \\\nc\nnext\n";
        let merged = merge_multiline_entries(contents);
        assert_eq!(merged, vec!["a \nb \nc".to_string(), "next".to_string()]);
    }

    #[test]
    fn test_fingerprint_differs_when_size_changes_same_mtime() {
        // Two files with the same mtime but different sizes must produce
        // different fingerprints — otherwise rapid edits land on the same
        // mtime second and the cache never refreshes.
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a");
        let p2 = dir.path().join("b");
        std::fs::write(&p1, b"hello").unwrap();
        std::fs::write(&p2, b"hello world").unwrap();

        let t = SystemTime::now();
        filetime::set_file_mtime(&p1, filetime::FileTime::from_system_time(t)).unwrap();
        filetime::set_file_mtime(&p2, filetime::FileTime::from_system_time(t)).unwrap();

        let f1 = FileFingerprint::from_path(&p1).unwrap();
        let f2 = FileFingerprint::from_path(&p2).unwrap();
        assert_eq!(f1.mtime, f2.mtime, "sanity: mtimes match");
        assert_ne!(f1, f2, "fingerprints must differ when size differs");
    }
}
