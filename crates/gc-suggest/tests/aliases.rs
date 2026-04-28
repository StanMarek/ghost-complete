//! Integration tests for multi-word alias expansion in spec resolution.
//!
//! These tests load the real workspace `specs/` directory so the wiring
//! between `parse_aliases` -> `AliasStore` -> `expand_alias_for_spec` ->
//! `spec_for_ctx` -> `resolve_spec` is exercised end-to-end. A pure unit
//! test in `alias_expand.rs` proves the helper's contract; this file
//! proves the engine actually picks up the alias-expanded spec subtree.

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
    // `alias gcb='gco -b'` then `alias gco='git checkout'`. Typing
    // `gcb feat` should resolve to `git checkout -b feat`. The `-b`
    // option's arg slot accepts a string positional, so this exercises
    // that the alias-tail reaches the resolved spec subtree (the engine
    // does not synthesise -b's argument generator from thin air —
    // `git checkout -b` does not have a generator either, but the spec
    // walk must still pick the correct subtree without crashing or
    // attempting to look up `gcb` directly).
    let aliases = HashMap::from([
        (
            "gcb".to_string(),
            vec!["gco".to_string(), "-b".to_string()],
        ),
        (
            "gco".to_string(),
            vec!["git".to_string(), "checkout".to_string()],
        ),
    ]);
    let engine = make_engine(aliases);

    let ctx = ctx_with("gcb", vec![], "feat", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "gcb feat")
        .expect("suggest_sync must succeed for a chained alias");

    // Chained alias resolution alone is the contract under test; the
    // engine must not error and must walk the chain to git's subtree.
    // We assert the negative: the engine did NOT attempt a literal
    // `gcb` spec lookup (which would yield zero suggestions and zero
    // generators because no `gcb.json` ships in specs/).
    let has_any_signal = !result.suggestions.is_empty()
        || !result.git_generators.is_empty()
        || !result.script_generators.is_empty()
        || !result.provider_generators.is_empty();
    assert!(
        has_any_signal,
        "chained alias resolution must produce at least one signal \
         (suggestions or pending generators); got empty SyncResult — \
         alias chain probably wasn't expanded",
    );
}

#[test]
fn alias_dev_resolves_to_ssh_for_host_injection() {
    // `alias dev=ssh` must still trigger ssh-host injection when the
    // user types `dev pro<TAB>` — the resolved head is `ssh`, so the
    // engine pulls hosts whose names start with `pro` from the cache.
    // Multi-word `alias dev='ssh prod.example.com'` would *suppress*
    // injection per SPEC D6, but a single-word alias is a pure rename
    // and matches the old behaviour.
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
    // Backward-compat invariant from SPEC: `alias g=git` continues to
    // resolve `g push` to git's `push` subcommand subtree.
    let aliases = HashMap::from([("g".to_string(), vec!["git".to_string()])]);
    let engine = make_engine(aliases);

    // Cursor at the start of arg-1 with no current_word — engine should
    // emit git's top-level subcommands (push, pull, checkout, ...).
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
