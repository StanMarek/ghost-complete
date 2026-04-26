//! Golden snapshot tests for the high-traffic commands covered by the
//! ranking + suppression contract: `git checkout`, `cargo run`, `cd`,
//! `git checkout ./`, `git checkout --`, `npm install`, `docker run`,
//! `ssh`, and `kubectl get`. Invariants are documented in
//! `docs/superpowers/specs/2026-04-25-completion-ranking-and-suppression-design.md`.
//!
//! Each test feeds a canonical buffer into `SuggestionEngine::suggest_sync`
//! and asserts an invariant about the resulting suggestions — usually that
//! certain kinds appear at the top, that filesystem entries are absent
//! entirely (no fs leak under spec-driven contexts), or that priorities are
//! honoured when they disagree with the alphabetical fallback. Intentionally
//! checks kinds and ordinal positions (not exact text) so the tests survive
//! spec content changes upstream.

use std::path::{Path, PathBuf};

use gc_buffer::parse_command_context;
use gc_suggest::commands::CommandsProvider;
use gc_suggest::history::HistoryProvider;
use gc_suggest::specs::SpecStore;
use gc_suggest::types::Suggestion;
use gc_suggest::{SuggestionEngine, SuggestionKind};

fn spec_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs")
}

fn build_engine() -> SuggestionEngine {
    let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
    let history = HistoryProvider::from_entries(vec![]);
    let commands = CommandsProvider::from_list(vec![]);
    SuggestionEngine::with_providers(spec_store, history, commands)
}

fn ctx_from(buffer: &str) -> gc_buffer::CommandContext {
    parse_command_context(buffer, buffer.chars().count())
}

fn tmp_cwd() -> PathBuf {
    let dir = std::env::temp_dir().join("ghost-complete-golden-tests");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[test]
fn git_checkout_no_query_ranks_branches_first() {
    let engine = build_engine();
    let buffer = "git checkout ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .take(3)
            .any(|s| s.kind == SuggestionKind::GitBranch)
            || !result.git_generators.is_empty(),
        "branches should be either visible or pending"
    );
}

#[test]
fn git_branch_priority_outranks_flag_priority() {
    // GitBranch base 80 > Flag base 30 — branches must sort above flags
    // when both share the empty-query path.
    let items = vec![
        Suggestion {
            text: "--force".to_string(),
            kind: SuggestionKind::Flag,
            priority: None,
            ..Default::default()
        },
        Suggestion {
            text: "main".to_string(),
            kind: SuggestionKind::GitBranch,
            priority: None,
            ..Default::default()
        },
    ];
    let result = gc_suggest::fuzzy::rank("", items, 50);
    assert_eq!(result[0].kind, SuggestionKind::GitBranch);
}

#[test]
fn cargo_run_no_query_no_filesystem_leak() {
    let engine = build_engine();
    let buffer = "cargo run ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "cargo run should not leak filesystem; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn cd_no_query_returns_directories_only() {
    let engine = build_engine();
    let buffer = "cd ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .all(|s| !matches!(s.kind, SuggestionKind::FilePath)),
        "cd should not return plain files; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn git_checkout_path_prefix_returns_filesystem_only() {
    let engine = build_engine();
    let buffer = "git checkout ./";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .all(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "PathPrefix context should yield filesystem only; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn git_checkout_flag_prefix_returns_flags_only() {
    let engine = build_engine();
    let buffer = "git checkout --";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .all(|s| matches!(s.kind, SuggestionKind::Flag | SuggestionKind::Subcommand)),
        "FlagPrefix context should yield flags/subcommands only; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn npm_install_no_filesystem_when_spec_omits_template() {
    let engine = build_engine();
    let buffer = "npm install ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "npm install spec lists no template; filesystem should not appear; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn docker_run_no_filesystem_when_spec_omits_template() {
    let engine = build_engine();
    let buffer = "docker run ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "docker run image arg has script generator; no fs leak expected; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn ssh_returns_hosts_or_pending_no_filesystem_leak() {
    let engine = build_engine();
    let buffer = "ssh ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "ssh arg has host generators; no fs leak expected; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn kubectl_get_no_filesystem_when_spec_provides_generators() {
    let engine = build_engine();
    let buffer = "kubectl get ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "kubectl get resource arg has generators; no fs leak expected; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

/// The audit pass set `cargo run` to priority 92 and `cargo add` to 90.
/// With an empty query the ranker has no fuzzy signal — only kind/priority,
/// then alphabetical tiebreak. Alphabetically `add` precedes `run`, so if
/// the priority plumbing were ever broken (both subcommands falling back to
/// the Subcommand kind base of 70 with no override), `add` would land
/// before `run`. Picking a pair where alphabetical order is REVERSED by
/// priority means this assertion only passes when priorities are actually
/// honoured end-to-end.
#[test]
fn cargo_high_priority_subcommand_outranks_alphabetical_neighbour() {
    let engine = build_engine();
    let buffer = "cargo ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();

    let position = |name: &str| {
        result
            .suggestions
            .iter()
            .position(|s| s.text == name && s.kind == SuggestionKind::Subcommand)
    };

    let run_pos = position("run").expect("`cargo run` subcommand should be suggested");
    let add_pos = position("add").expect("`cargo add` subcommand should be suggested");

    assert!(
        run_pos < add_pos,
        "cargo run (priority 92) must rank before cargo add (priority 90); \
         alphabetical fallback would put `add` first, so this only holds \
         when priorities are honoured. got run at {run_pos}, add at {add_pos}: {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind, s.priority))
            .collect::<Vec<_>>()
    );
}
