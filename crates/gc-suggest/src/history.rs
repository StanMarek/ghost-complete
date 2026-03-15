use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::provider::Provider;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

pub(crate) const DEFAULT_MAX_HISTORY_ENTRIES: usize = 10_000;

pub struct HistoryProvider {
    entries: Vec<String>,
}

impl HistoryProvider {
    pub fn load(max_entries: usize) -> Self {
        let entries = match Self::read_history(max_entries) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::debug!("failed to load history: {e}");
                Vec::new()
            }
        };
        Self { entries }
    }

    /// Test/bench constructor — inject entries directly.
    pub fn from_entries(entries: Vec<String>) -> Self {
        Self { entries }
    }

    fn read_history(max_entries: usize) -> Result<Vec<String>> {
        let path = Self::history_path()?;
        let raw = std::fs::read(&path)?;
        let contents = String::from_utf8_lossy(&raw);
        Ok(Self::parse_and_dedup(&contents, max_entries))
    }

    fn history_path() -> Result<std::path::PathBuf> {
        // Check $HISTFILE first, fall back to ~/.zsh_history
        if let Ok(histfile) = std::env::var("HISTFILE") {
            return Ok(std::path::PathBuf::from(histfile));
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

        let suggestions = self
            .entries
            .iter()
            .map(|entry| {
                Suggestion {
                    text: entry.clone(),
                    description: None,
                    kind: SuggestionKind::History,
                    source: SuggestionSource::History,
                    ..Default::default()
                }
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
        assert!(results.is_empty(), "history should be empty in pipe segment");
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
}
