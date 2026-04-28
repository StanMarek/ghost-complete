//! Integration tests: alias map -> spec resolution wiring against real specs/.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gc_buffer::{CommandContext, QuoteState};
use gc_suggest::commands::CommandsProvider;
use gc_suggest::git::GitQueryKind;
use gc_suggest::history::HistoryProvider;
use gc_suggest::types::SuggestionSource;
use gc_suggest::{SpecStore, SuggestionEngine};

/// CARGO_MANIFEST_DIR is the crate root, so walk up two levels to the workspace specs/.
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
    let aliases = HashMap::from([(
        "gco".to_string(),
        vec!["git".to_string(), "checkout".to_string()],
    )]);
    let engine = make_engine(aliases);

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

#[test]
fn alias_gco_flag_prefix_walks_git_checkout_options() {
    // alias gco='git checkout': typing `gco -<TAB>` must surface git checkout's
    // flag set (e.g. -b, -B, -t, --track), not git's top-level flags
    // (--git-dir, --exec-path, --no-pager).
    let aliases = HashMap::from([(
        "gco".to_string(),
        vec!["git".to_string(), "checkout".to_string()],
    )]);
    let engine = make_engine(aliases);

    let ctx = ctx_with("gco", vec![], "-", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "gco -")
        .expect("suggest_sync must succeed");

    let texts: Vec<&str> = result.suggestions.iter().map(|s| s.text.as_str()).collect();
    let has_checkout_flag = texts
        .iter()
        .any(|t| matches!(*t, "-b" | "-B" | "-t" | "--track"));
    assert!(
        has_checkout_flag,
        "alias gco='git checkout' must surface git checkout flags (-b/-B/-t/--track); got {texts:?}",
    );
    let has_top_level_only = texts
        .iter()
        .any(|t| matches!(*t, "--git-dir" | "--exec-path" | "--no-pager"));
    assert!(
        !has_top_level_only,
        "alias gco='git checkout' must not leak git's top-level flags; got {texts:?}",
    );
}

#[test]
fn alias_to_unknown_command_does_not_leak_alias_name_spec() {
    // alias git=somecmd-with-no-spec must NOT resolve to git's spec.
    let aliases = HashMap::from([("git".to_string(), vec!["somecmd-with-no-spec".to_string()])]);
    let engine = make_engine(aliases);

    let ctx = ctx_with("git", vec![], "foo", 1);
    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "git foo")
        .expect("suggest_sync must succeed");

    let texts: Vec<&str> = result.suggestions.iter().map(|s| s.text.as_str()).collect();
    let has_git_subcommand = texts
        .iter()
        .any(|t| matches!(*t, "checkout" | "rebase" | "branch" | "commit"));
    assert!(
        !has_git_subcommand,
        "alias to an unknown spec must not leak git's subcommands; got {texts:?}",
    );
}
