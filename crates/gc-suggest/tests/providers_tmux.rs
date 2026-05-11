pub use gc_suggest::providers::{Provider, ProviderCtx};

mod types {
    pub use gc_suggest::types::*;
}

mod providers {
    pub mod util {
        include!("../src/providers/util.rs");
    }
}

mod util {
    pub(crate) use crate::providers::util::*;
}

#[path = "../src/providers/tmux_state.rs"]
mod tmux_state;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use tmux_state::{parse_tmux_state_output, TmuxClients, TmuxPanes, TmuxSessions, TmuxWindows};

fn ctx_with_env(env: &[(&str, &str)]) -> ProviderCtx {
    ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(
            env.iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect::<HashMap<_, _>>(),
        ),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    }
}

#[test]
fn provider_names_match_spec_type_strings() {
    assert_eq!(TmuxSessions.name(), "tmux_sessions");
    assert_eq!(TmuxWindows.name(), "tmux_windows");
    assert_eq!(TmuxPanes.name(), "tmux_panes");
    assert_eq!(TmuxClients.name(), "tmux_clients");
}

#[test]
fn parses_recorded_tmux_formatted_output() {
    let suggestions = parse_tmux_state_output(
        "main\t2 windows\n\
         work:1:editor\tactive window\n\
         work:1.0\tvim\n\
         /dev/ttys003\twork\n",
    );

    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["main", "work:1:editor", "work:1.0", "/dev/ttys003"]
    );
    assert_eq!(suggestions[0].description.as_deref(), Some("2 windows"));
    assert_eq!(suggestions[1].description.as_deref(), Some("active window"));
    assert_eq!(suggestions[2].description.as_deref(), Some("vim"));
    assert_eq!(suggestions[3].description.as_deref(), Some("work"));
    for suggestion in &suggestions {
        assert_eq!(suggestion.kind, gc_suggest::SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, gc_suggest::SuggestionSource::Provider);
    }
}

#[test]
fn parses_empty_tmux_output_as_empty() {
    assert!(parse_tmux_state_output("").is_empty());
    assert!(parse_tmux_state_output("\n\n  \n").is_empty());
}

#[test]
fn drops_malformed_tmux_rows_without_targets() {
    let suggestions = parse_tmux_state_output("\tmissing target\n   \talso missing\n\n");

    assert!(suggestions.is_empty());
}

#[tokio::test]
async fn generate_skips_when_tmux_env_unset_even_if_binary_is_missing() {
    let ctx = ctx_with_env(&[]);

    let result = TmuxSessions
        .generate_with_binary(&ctx, "/nonexistent/tmux-for-test")
        .await;

    assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
}

#[tokio::test]
async fn generate_returns_empty_when_tmux_binary_is_missing() {
    let ctx = ctx_with_env(&[("TMUX", "/tmp/tmux-501/default,123,0")]);

    let result = TmuxSessions
        .generate_with_binary(&ctx, "/nonexistent/tmux-for-test")
        .await;

    assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
}
