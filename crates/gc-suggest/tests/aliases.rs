//! Integration tests: alias map -> spec resolution wiring against real specs/.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gc_buffer::{CommandContext, QuoteState};
use gc_suggest::commands::CommandsProvider;
use gc_suggest::git::GitQueryKind;
use gc_suggest::history::HistoryProvider;
use gc_suggest::types::SuggestionSource;
use gc_suggest::{SpecStore, SuggestionEngine};

/// Path to the workspace's `specs/` directory. Cargo runs integration
/// tests with `CARGO_MANIFEST_DIR` set to the crate root, so we walk up
/// two levels to reach the workspace root.
fn workspace_specs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs")
}

fn ctx_with(command: &str, args: Vec<&str>, current: &str, word_index: usize) -> CommandContext {
    CommandContext {
        command: Some(command.to_string()),
        args: args.into_iter().map(String::from).collect(),
        current_word: current.to_string(),
        word_index,
        is_flag: current.starts_with('-'),
        is_long_flag: current.starts_with("--"),
        preceding_flag: None,
        in_pipe: false,
        in_redirect: false,
        quote_state: QuoteState::None,
        is_first_segment: true,
    }
}

fn make_engine(aliases: HashMap<String, Vec<String>>) -> SuggestionEngine {
    let store = SpecStore::load_from_dir(&workspace_specs_dir())
        .expect("workspace specs/ must load")
        .store;
    SuggestionEngine::with_providers(
        store,
        HistoryProvider::from_entries(vec![]),
        CommandsProvider::from_list(vec![]),
    )
    .with_aliases(aliases)
}

#[test]
fn alias_gco_resolves_to_git_checkout_branches() {
    // `alias gco='git checkout'` is the canonical failure case. Without
    // expansion, `gco main` lands on git's top-level subcommand list and
    // never reaches the checkout subtree's git_branches generator.
    let aliases = HashMap::from([(
        "gco".to_string(),
        vec!["git".to_string(), "checkout".to_string()],
    )]);
    let engine = make_engine(aliases);

    // Cursor sits on `main` after `gco `: command="gco", args=[],
    // current_word="main", word_index=1.
    let ctx = ctx_with("gco", vec![], "main", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "gco main")
        .expect("suggest_sync must not error on a valid alias");

    assert!(
        result.git_generators.contains(&GitQueryKind::Branches),
        "alias gco='git checkout' must dispatch git_branches; got {:?}",
        result.git_generators,
    );
}

#[test]
fn alias_chained_gcb_resolves_to_git_checkout_b() {
    // gcb -> gco -> git checkout: chained expansion must reach git's spec subtree.
    let aliases = HashMap::from([
        ("gcb".to_string(), vec!["gco".to_string(), "-b".to_string()]),
        (
            "gco".to_string(),
            vec!["git".to_string(), "checkout".to_string()],
        ),
    ]);
    let engine = make_engine(aliases);

    let ctx = ctx_with("gcb", vec![], "main", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "gcb main")
        .expect("suggest_sync must succeed for a chained alias");

    assert!(
        result.git_generators.contains(&GitQueryKind::Branches),
        "chained alias gcb -> gco -> git checkout must dispatch git_branches; got {:?}",
        result.git_generators,
    );
}

#[test]
fn alias_dev_resolves_to_ssh_for_host_injection() {
    // alias dev=ssh: resolved head is `ssh`, so ssh-host injection still fires.
    let ssh_dir = tempfile::tempdir().expect("tempdir for ssh fixture");
    let ssh_config = ssh_dir.path().join("config");
    std::fs::write(
        &ssh_config,
        "Host prod-east\n    HostName 1.2.3.4\n\nHost prod-west\n\nHost staging\n",
    )
    .expect("write ssh fixture");

    let aliases = HashMap::from([("dev".to_string(), vec!["ssh".to_string()])]);
    let engine = make_engine(aliases).with_ssh_host_cache_path(ssh_config);

    let ctx = ctx_with("dev", vec![], "pro", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "dev pro")
        .expect("suggest_sync must succeed");

    let ssh_hits: Vec<_> = result
        .suggestions
        .iter()
        .filter(|s| s.source == SuggestionSource::SshConfig)
        .map(|s| s.text.clone())
        .collect();
    assert!(
        !ssh_hits.is_empty(),
        "alias dev=ssh must surface SshConfig hosts matching `pro`; \
         got suggestions {:?}",
        result.suggestions,
    );
    assert!(
        ssh_hits.iter().any(|h| h.starts_with("pro")),
        "every host returned must match the prefix `pro`; got {ssh_hits:?}",
    );
}

#[test]
fn single_word_alias_still_works() {
    let aliases = HashMap::from([("g".to_string(), vec!["git".to_string()])]);
    let engine = make_engine(aliases);

    let ctx = ctx_with("g", vec![], "", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "g ")
        .expect("suggest_sync must succeed");

    let has_subcommand = result
        .suggestions
        .iter()
        .any(|s| s.text == "push" || s.text == "pull" || s.text == "checkout");
    assert!(
        has_subcommand,
        "alias g=git must surface git's top-level subcommands; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| &s.text)
            .collect::<Vec<_>>(),
    );
}

#[test]
fn alias_shadows_spec_with_same_name() {
    // alias git=ls must redirect resolution to ls's spec, not git's subcommands.
    let aliases = HashMap::from([("git".to_string(), vec!["ls".to_string()])]);
    let engine = make_engine(aliases);

    let ctx = ctx_with("git", vec![], "-", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "git -")
        .expect("suggest_sync must succeed");

    let texts: Vec<&str> = result.suggestions.iter().map(|s| s.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| *t == "-a" || *t == "-l"),
        "alias git=ls must surface ls flags like -a/-l; got {texts:?}",
    );
    assert!(
        !texts.iter().any(|t| *t == "checkout" || *t == "rebase"),
        "alias git=ls must not leak git subcommands; got {texts:?}",
    );
}

#[test]
fn alias_dev_to_ssh_walks_ssh_spec_subtree() {
    let aliases = HashMap::from([("dev".to_string(), vec!["ssh".to_string()])]);
    let engine = make_engine(aliases);

    let ctx = ctx_with("dev", vec![], "-", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "dev -")
        .expect("suggest_sync must succeed");

    let texts: Vec<&str> = result.suggestions.iter().map(|s| s.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| *t == "-p" || *t == "-l"),
        "alias dev=ssh must surface ssh option flags like -p/-l; got {texts:?}",
    );
}
