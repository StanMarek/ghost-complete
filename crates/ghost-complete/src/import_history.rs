//! `ghost-complete import-history` — seed the frecency database from a
//! zsh history file so commonly-used commands rank highly from day one.
//!
//! For each history line we tokenize on whitespace, split the line into
//! pipeline segments (`|`, `&&`, `||`, `;`), and for each segment record:
//! - One `Command`-position frecency hit on the head token (e.g. `git`).
//! - One `Subcommand`-position frecency hit on the next non-flag,
//!   non-path-looking token (e.g. `push` after `git`). When the second
//!   token starts with `-/.~$=` we skip it — those buckets are
//!   command-specific and would just clutter the frecency JSON.
//!
//! Multi-line history entries are merged by [`HistoryProvider::read_history_from`]
//! before we see them.

use anyhow::{Context, Result};
use std::path::PathBuf;

use gc_suggest::frecency::{frecency_key, FrecencyDb};
use gc_suggest::history::HistoryProvider;
use gc_suggest::types::SuggestionKind;

const PIPELINE_SEPARATORS: &[&str] = &["|", "&&", "||", ";"];

/// Mirror of `gc_suggest::history::DEFAULT_MAX_HISTORY_ENTRIES` (which is
/// `pub(crate)` over there). Keeping a local constant avoids broadening
/// the gc-suggest API surface for a one-off importer cap default.
const DEFAULT_MAX_HISTORY_ENTRIES: usize = 10_000;

pub fn run_import_history(
    path_override: Option<&str>,
    max_entries: Option<usize>,
    dry_run: bool,
) -> Result<()> {
    let history_path = resolve_history_path(path_override)?;
    let cap = max_entries.unwrap_or(DEFAULT_MAX_HISTORY_ENTRIES);

    println!("Reading {} (cap: {} entries)…", history_path.display(), cap);
    let entries = HistoryProvider::read_history_from(&history_path, cap)
        .with_context(|| format!("failed to read history at {}", history_path.display()))?;
    println!("  Found {} unique entries.", entries.len());

    let recordings = build_recordings(&entries);
    println!(
        "  Generated {} frecency records ({} commands, {} subcommands).",
        recordings.len(),
        recordings
            .iter()
            .filter(|r| r.kind == SuggestionKind::Command)
            .count(),
        recordings
            .iter()
            .filter(|r| r.kind == SuggestionKind::Subcommand)
            .count(),
    );

    if dry_run {
        println!("\n[dry-run] Skipping write. First 10 records:");
        for r in recordings.iter().take(10) {
            println!("  {r}");
        }
        return Ok(());
    }

    let db = FrecencyDb::load();
    for r in &recordings {
        db.record(&frecency_key(r.command.as_deref(), r.kind, &r.text));
    }
    db.flush();

    println!(
        "\n\x1b[32m\u{2713}\x1b[0m  Imported {} records into the frecency store.",
        recordings.len()
    );
    println!(
        "  Tip: open a new shell to attach to a fresh proxy that loads the updated scores."
    );
    Ok(())
}

#[derive(Debug)]
struct Recording {
    command: Option<String>,
    kind: SuggestionKind,
    text: String,
}

impl std::fmt::Display for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.command, self.kind) {
            (None, SuggestionKind::Command) => write!(f, "cmd  {}", self.text),
            (Some(c), SuggestionKind::Subcommand) => write!(f, "sub  {} {}", c, self.text),
            _ => write!(f, "{:?} {:?} {}", self.command, self.kind, self.text),
        }
    }
}

fn build_recordings(entries: &[String]) -> Vec<Recording> {
    let mut out = Vec::new();
    for line in entries {
        for segment in split_pipeline(line) {
            let mut tokens = segment.split_whitespace();
            let Some(cmd) = tokens.next() else {
                continue;
            };
            if !is_plausible_command(cmd) {
                continue;
            }
            out.push(Recording {
                command: None,
                kind: SuggestionKind::Command,
                text: cmd.to_string(),
            });
            if let Some(sub) = tokens.find(|t| !t.is_empty()) {
                if is_plausible_subcommand(sub) {
                    out.push(Recording {
                        command: Some(cmd.to_string()),
                        kind: SuggestionKind::Subcommand,
                        text: sub.to_string(),
                    });
                }
            }
        }
    }
    out
}

fn split_pipeline(line: &str) -> Vec<&str> {
    let mut segments = vec![line];
    for sep in PIPELINE_SEPARATORS {
        segments = segments
            .into_iter()
            .flat_map(|s| s.split(sep))
            .collect();
    }
    segments
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn is_plausible_command(token: &str) -> bool {
    let first = token.chars().next().unwrap_or('\0');
    !matches!(first, '-' | '/' | '.' | '~' | '$' | '=' | '\\' | '(')
        && !token.contains('=')
}

fn is_plausible_subcommand(token: &str) -> bool {
    let first = token.chars().next().unwrap_or('\0');
    !matches!(first, '-' | '/' | '.' | '~' | '$' | '=' | '\\')
}

fn resolve_history_path(override_: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = override_ {
        return Ok(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("HISTFILE") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".zsh_history"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn build_recordings_emits_command_and_subcommand_pairs() {
        let entries = vec![entry("git push origin main"), entry("cargo build")];
        let recordings = build_recordings(&entries);
        let pairs: Vec<(Option<&str>, SuggestionKind, &str)> = recordings
            .iter()
            .map(|r| (r.command.as_deref(), r.kind, r.text.as_str()))
            .collect();
        assert!(pairs.contains(&(None, SuggestionKind::Command, "git")));
        assert!(pairs.contains(&(Some("git"), SuggestionKind::Subcommand, "push")));
        assert!(pairs.contains(&(None, SuggestionKind::Command, "cargo")));
        assert!(pairs.contains(&(Some("cargo"), SuggestionKind::Subcommand, "build")));
    }

    #[test]
    fn build_recordings_splits_pipelines() {
        let entries = vec![entry("cat /tmp/log | grep error && echo done")];
        let recordings = build_recordings(&entries);
        let cmds: Vec<&str> = recordings
            .iter()
            .filter(|r| r.kind == SuggestionKind::Command)
            .map(|r| r.text.as_str())
            .collect();
        assert!(cmds.contains(&"cat"));
        assert!(cmds.contains(&"grep"));
        assert!(cmds.contains(&"echo"));
    }

    #[test]
    fn build_recordings_skips_path_like_subcommand_tokens() {
        let entries = vec![entry("ls /usr/local/bin")];
        let recordings = build_recordings(&entries);
        assert!(recordings
            .iter()
            .any(|r| r.kind == SuggestionKind::Command && r.text == "ls"));
        assert!(!recordings
            .iter()
            .any(|r| r.kind == SuggestionKind::Subcommand));
    }

    #[test]
    fn build_recordings_skips_assignment_lines() {
        // VAR=value commands aren't useful frecency seeds.
        let entries = vec![entry("FOO=bar")];
        let recordings = build_recordings(&entries);
        assert!(recordings.is_empty());
    }

    #[test]
    fn build_recordings_skips_dash_subcommands() {
        // First non-cmd token is a flag — no subcommand bucket should be recorded.
        let entries = vec![entry("ls -la")];
        let recordings = build_recordings(&entries);
        assert_eq!(
            recordings
                .iter()
                .filter(|r| r.kind == SuggestionKind::Subcommand)
                .count(),
            0
        );
    }
}
