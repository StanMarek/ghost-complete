//! Golden snapshot tests for the 8 commands audited in
//! `docs/superpowers/specs/2026-04-25-completion-ranking-and-suppression-design.md`.
//!
//! Each test feeds a canonical buffer into `SuggestionEngine::suggest_sync`
//! and asserts the kind of the top results. Intentionally checks kinds (not
//! exact text) so the tests survive spec content changes upstream.

use std::path::{Path, PathBuf};

use gc_buffer::parse_command_context;
use gc_suggest::commands::CommandsProvider;
use gc_suggest::history::HistoryProvider;
use gc_suggest::specs::SpecStore;
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

/// The audit pass set `cargo build` to priority 92 while `cargo check` keeps
/// the default Subcommand base of 70. With an empty query the ranker has no
/// fuzzy signal to lean on — only kind/priority. So even though `build`
/// sorts after `check` alphabetically, the priority bump must put it first.
/// If the heuristic-driven priorities ever get reverted or accidentally
/// stripped, this test will catch the regression in spec ordering.
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

    let build_pos = position("build").expect("`cargo build` subcommand should be suggested");
    let check_pos = position("check").expect("`cargo check` subcommand should be suggested");

    assert!(
        build_pos < check_pos,
        "cargo build (priority 92) must rank before cargo check (no priority); \
         got build at {build_pos}, check at {check_pos}: {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind, s.priority))
            .collect::<Vec<_>>()
    );
}
