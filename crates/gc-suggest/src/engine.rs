use std::path::Path;

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::commands::CommandsProvider;
use crate::filesystem::FilesystemProvider;
use crate::fuzzy;
use crate::git;
use crate::history::HistoryProvider;
use crate::provider::Provider;
use crate::specs::{self, SpecStore};
use crate::types::Suggestion;

pub struct SuggestionEngine {
    spec_store: SpecStore,
    filesystem_provider: FilesystemProvider,
    history_provider: HistoryProvider,
    commands_provider: CommandsProvider,
}

impl SuggestionEngine {
    pub fn new(spec_dir: &Path) -> Result<Self> {
        Ok(Self {
            spec_store: SpecStore::load_from_dir(spec_dir)?,
            filesystem_provider: FilesystemProvider::new(),
            history_provider: HistoryProvider::load(),
            commands_provider: CommandsProvider::from_path_env(),
        })
    }

    #[cfg(test)]
    fn with_providers(
        spec_store: SpecStore,
        history_provider: HistoryProvider,
        commands_provider: CommandsProvider,
    ) -> Self {
        Self {
            spec_store,
            filesystem_provider: FilesystemProvider::new(),
            history_provider,
            commands_provider,
        }
    }

    pub fn suggest_sync(&self, ctx: &CommandContext, cwd: &Path) -> Result<Vec<Suggestion>> {
        let mut candidates = Vec::new();

        // Command position: commands + history
        if ctx.word_index == 0 {
            if let Ok(cmds) = self.commands_provider.provide(ctx, cwd) {
                candidates.extend(cmds);
            }
            if let Ok(hist) = self.history_provider.provide(ctx, cwd) {
                candidates.extend(hist);
            }
            return Ok(fuzzy::rank(&ctx.current_word, candidates));
        }

        // Redirect: always filesystem
        if ctx.in_redirect {
            if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
                candidates.extend(fs);
            }
            return Ok(fuzzy::rank(&ctx.current_word, candidates));
        }

        // Path-like current_word: filesystem
        if looks_like_path(&ctx.current_word) {
            if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
                candidates.extend(fs);
            }
            return Ok(fuzzy::rank(&ctx.current_word, candidates));
        }

        // Check for a spec for this command
        if let Some(command) = &ctx.command {
            if let Some(spec) = self.spec_store.get(command) {
                let resolution = specs::resolve_spec(spec, ctx);

                // Add subcommands and options from the spec
                candidates.extend(resolution.subcommands);
                candidates.extend(resolution.options);

                // Handle generators (e.g., git branches/tags/remotes)
                for gen_type in &resolution.generators {
                    if let Some(kind) = git::generator_to_query_kind(gen_type) {
                        if let Ok(git_suggestions) = git::git_suggestions(cwd, kind) {
                            candidates.extend(git_suggestions);
                        }
                    }
                }

                // Add filesystem if spec wants filepaths
                if resolution.wants_filepaths {
                    if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
                        candidates.extend(fs);
                    }
                }

                return Ok(fuzzy::rank(&ctx.current_word, candidates));
            }
        }

        // No spec — fallback to filesystem
        if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
            candidates.extend(fs);
        }
        Ok(fuzzy::rank(&ctx.current_word, candidates))
    }
}

fn looks_like_path(word: &str) -> bool {
    word.contains('/') || word.starts_with('.') || word.starts_with('~')
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_buffer::QuoteState;
    use std::path::PathBuf;

    fn spec_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs")
    }

    fn make_engine() -> SuggestionEngine {
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap();
        let history = HistoryProvider::from_entries(vec![
            "git push".into(),
            "cargo build".into(),
            "ls -la".into(),
        ]);
        let commands = CommandsProvider::from_list(vec!["git".into(), "ls".into(), "cargo".into()]);
        SuggestionEngine::with_providers(spec_store, history, commands)
    }

    fn make_ctx(
        command: Option<&str>,
        args: Vec<&str>,
        current_word: &str,
        word_index: usize,
    ) -> CommandContext {
        CommandContext {
            command: command.map(String::from),
            args: args.into_iter().map(String::from).collect(),
            current_word: current_word.to_string(),
            word_index,
            is_flag: current_word.starts_with('-'),
            is_long_flag: current_word.starts_with("--"),
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
        }
    }

    #[test]
    fn test_command_position_returns_commands_and_history() {
        let engine = make_engine();
        let ctx = make_ctx(None, vec![], "gi", 0);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        // Should have "git" from both commands and history
        assert!(results.iter().any(|s| s.text == "git"));
    }

    #[test]
    fn test_spec_subcommands() {
        let engine = make_engine();
        let ctx = make_ctx(Some("git"), vec![], "ch", 1);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.iter().any(|s| s.text == "checkout"),
            "expected 'checkout' in results: {results:?}"
        );
    }

    #[test]
    fn test_spec_options() {
        let engine = make_engine();
        // Query "--" should match long flags like --message, --amend, etc.
        let ctx = make_ctx(Some("git"), vec!["commit"], "--", 2);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.iter().any(|s| s.text == "--message"),
            "expected '--message' in results: {results:?}"
        );
        assert!(
            results.iter().any(|s| s.text == "--amend"),
            "expected '--amend' in results: {results:?}"
        );

        // Query "-" should match short flags like -m, -a
        let ctx = make_ctx(Some("git"), vec!["commit"], "-", 2);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.iter().any(|s| s.text == "-m"),
            "expected '-m' in results: {results:?}"
        );
    }

    #[test]
    fn test_redirect_gives_filesystem() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("output.txt"), "").unwrap();
        let mut ctx = make_ctx(Some("echo"), vec!["hello"], "", 2);
        ctx.in_redirect = true;
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(results.iter().any(|s| s.text == "output.txt"));
    }

    #[test]
    fn test_path_prefix_triggers_filesystem() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "").unwrap();
        let ctx = make_ctx(Some("cat"), vec![], "src/", 1);
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(
            results.iter().any(|s| s.text == "src/main.rs"),
            "expected 'src/main.rs' in results: {results:?}"
        );
    }

    #[test]
    fn test_unknown_command_falls_back_to_filesystem() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("data.csv"), "").unwrap();
        let ctx = make_ctx(Some("unknown_cmd"), vec![], "", 1);
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(results.iter().any(|s| s.text == "data.csv"));
    }

    #[test]
    fn test_empty_results_for_no_matches() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = make_ctx(Some("git"), vec![], "zzzzzzz_no_match", 1);
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(results.is_empty());
    }
}
