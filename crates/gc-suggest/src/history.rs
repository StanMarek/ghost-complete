use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::provider::Provider;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

pub(crate) const DEFAULT_MAX_HISTORY_ENTRIES: usize = 10_000;

pub struct HistoryProvider {
    state: Mutex<HistoryState>,
    /// `None` for test/bench constructors — never refreshes.
    path: Option<PathBuf>,
    max_entries: usize,
}

struct HistoryState {
    entries: Vec<String>,
    mtime: Option<SystemTime>,
}

impl HistoryProvider {
    pub fn load(max_entries: usize) -> Self {
        let path = Self::history_path().ok();
        let (entries, mtime) = match &path {
            Some(p) => {
                let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
                match Self::read_history_from(p, max_entries) {
                    Ok(entries) => (entries, mtime),
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
            state: Mutex::new(HistoryState { entries, mtime }),
            path,
            max_entries,
        }
    }

    /// Test/bench constructor — inject entries directly. Never refreshes.
    pub fn from_entries(entries: Vec<String>) -> Self {
        Self {
            state: Mutex::new(HistoryState {
                entries,
                mtime: None,
            }),
            path: None,
            max_entries: 0,
        }
    }

    /// Re-read the history file if its mtime has changed.
    /// Does nothing if the provider was created via `from_entries()`.
    fn refresh_if_stale(&self) {
        let path = match &self.path {
            Some(p) => p,
            None => return,
        };

        let current_mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return, // can't stat — keep existing entries
        };

        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return, // poisoned — keep stale entries rather than panic
        };
        if state.mtime == Some(current_mtime) {
            return; // unchanged
        }

        match Self::read_history_from(path, self.max_entries) {
            Ok(entries) => {
                state.entries = entries;
                state.mtime = Some(current_mtime);
            }
            Err(e) => {
                tracing::debug!("failed to refresh history: {e}");
                // keep existing entries, but update mtime so we don't retry every call
                state.mtime = Some(current_mtime);
            }
        }
    }

    fn read_history_from(path: &Path, max_entries: usize) -> Result<Vec<String>> {
        let raw = std::fs::read(path)?;
        let contents = String::from_utf8_lossy(&raw);
        Ok(Self::parse_and_dedup(&contents, max_entries))
    }

    fn history_path() -> Result<PathBuf> {
        // Check $HISTFILE first, fall back to ~/.zsh_history
        if let Ok(histfile) = std::env::var("HISTFILE") {
            return Ok(PathBuf::from(histfile));
        }
        if let Some(home) = dirs::home_dir() {
            return Ok(home.join(".zsh_history"));
        }
        anyhow::bail!("could not determine history file path")
    }

    fn parse_and_dedup(contents: &str, max_entries: usize) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut entries = Vec::new();

        // Process lines in reverse so we keep the most recent occurrence
        for line in contents.lines().rev() {
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
            Err(_) => return Ok(Vec::new()), // poisoned — return empty rather than panic
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

    fn name(&self) -> &'static str {
        "history"
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
                mtime: std::fs::metadata(&hist_path)
                    .and_then(|m| m.modified())
                    .ok(),
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
}
