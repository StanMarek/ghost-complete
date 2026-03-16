use std::path::Path;

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::provider::Provider;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

pub struct EnvProvider;

impl EnvProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Provider for EnvProvider {
    fn provide(&self, ctx: &CommandContext, _cwd: &Path) -> Result<Vec<Suggestion>> {
        if !ctx.current_word.starts_with('$') {
            return Ok(Vec::new());
        }

        // Pre-filter: only allocate suggestions for vars matching the typed
        // prefix (e.g. "$PA" → only vars starting with "PA"). Avoids creating
        // hundreds of Suggestion objects on systems with large environments.
        let typed_prefix = &ctx.current_word[1..]; // strip leading '$'

        let suggestions = std::env::vars()
            .filter(|(key, _)| typed_prefix.is_empty() || key.starts_with(typed_prefix))
            .map(|(key, _)| Suggestion {
                text: format!("${key}"),
                description: None,
                kind: SuggestionKind::EnvVar,
                source: SuggestionSource::Env,
                score: 0,
                match_indices: Vec::new(),
            })
            .collect();

        Ok(suggestions)
    }

    fn name(&self) -> &'static str {
        "env"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_buffer::QuoteState;

    fn make_ctx(current_word: &str, word_index: usize) -> CommandContext {
        CommandContext {
            command: Some("echo".into()),
            args: vec![],
            current_word: current_word.to_string(),
            word_index,
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
    fn test_provides_suggestions_when_dollar_prefix() {
        // Use $HOME which always exists — avoids set_var entirely
        let provider = EnvProvider::new();
        let ctx = make_ctx("$", 1);
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            !results.is_empty(),
            "should return env var suggestions when current_word starts with $"
        );
    }

    #[test]
    fn test_empty_when_no_dollar_prefix() {
        let provider = EnvProvider::new();
        let ctx = make_ctx("HOME", 1);
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.is_empty(),
            "should return empty when current_word does not start with $"
        );
    }

    #[test]
    fn test_suggestions_have_dollar_prefix() {
        // Filter for $HOME which always exists
        let provider = EnvProvider::new();
        let ctx = make_ctx("$HOM", 1);
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.iter().all(|s| s.text.starts_with('$')),
            "all suggestions should have $ prefix in their text"
        );
        assert!(
            results.iter().any(|s| s.text == "$HOME"),
            "should contain $HOME: {:?}",
            results.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_suggestions_have_correct_kind_and_source() {
        let provider = EnvProvider::new();
        let ctx = make_ctx("$", 1);
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        for s in &results {
            assert_eq!(s.kind, SuggestionKind::EnvVar);
            assert_eq!(s.source, SuggestionSource::Env);
        }
    }

    #[test]
    fn test_prefix_filter_reduces_results() {
        let provider = EnvProvider::new();
        let all = provider
            .provide(&make_ctx("$", 1), Path::new("/tmp"))
            .unwrap();
        let filtered = provider
            .provide(&make_ctx("$HOM", 1), Path::new("/tmp"))
            .unwrap();
        assert!(
            filtered.len() <= all.len(),
            "prefix filtering should reduce results"
        );
        assert!(
            filtered.iter().all(|s| s.text.starts_with("$HOM")),
            "all filtered results should match prefix"
        );
    }
}
