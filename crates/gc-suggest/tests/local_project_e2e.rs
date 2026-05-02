//! End-to-end tests for the local-project providers, drilling through
//! `SuggestionEngine::resolve_providers` (the same async dispatcher the
//! handler invokes off the keystroke hot path). Each test stages a
//! fixture project in a tempdir, builds a `ProviderCtx` rooted at that
//! tempdir, and asserts the expected suggestions surface with the
//! correct `kind` + `source`.
//!
//! The full spec-routing path (`{"type": "..."}` in a JSON spec →
//! `ProviderKind` → dispatcher) is covered by
//! `test_resolve_spec_routes_known_provider_to_provider_generators` in
//! `crates/gc-suggest/src/specs.rs` for every registered provider
//! string. These tests cover the runtime dispatcher half of that
//! contract for the three new local-project providers specifically.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use gc_suggest::commands::CommandsProvider;
use gc_suggest::history::HistoryProvider;
use gc_suggest::providers::{ProviderCtx, ProviderKind};
use gc_suggest::specs::SpecStore;
use gc_suggest::types::{SuggestionKind, SuggestionSource};
use gc_suggest::SuggestionEngine;
use tempfile::TempDir;

fn engine() -> SuggestionEngine {
    let spec_store = SpecStore::load_from_dirs(&[]).unwrap().store;
    let history = HistoryProvider::from_entries(vec![]);
    let commands = CommandsProvider::from_list(vec![]);
    SuggestionEngine::with_providers(spec_store, history, commands)
}

fn ctx_for(cwd: &Path) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
    }
}

#[tokio::test]
async fn make_tab_lists_makefile_targets() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Makefile"),
        b"build:\n\ttouch x\ntest:\n\tcargo test\n",
    )
    .unwrap();

    let engine = engine();
    let ctx = ctx_for(tmp.path());
    let suggestions = engine
        .resolve_providers(&[ProviderKind::MakefileTargets], &ctx, "")
        .await
        .unwrap();
    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert!(texts.contains(&"build"));
    assert!(texts.contains(&"test"));
    for s in &suggestions {
        assert_eq!(s.kind, SuggestionKind::ProviderValue);
        assert_eq!(s.source, SuggestionSource::Provider);
    }
}

#[tokio::test]
async fn npm_run_tab_lists_package_scripts() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        br#"{"scripts": {"start": "node x.js", "lint": "eslint ."}}"#,
    )
    .unwrap();

    let engine = engine();
    let ctx = ctx_for(tmp.path());
    let suggestions = engine
        .resolve_providers(&[ProviderKind::NpmScripts], &ctx, "")
        .await
        .unwrap();
    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert!(texts.contains(&"start"));
    assert!(texts.contains(&"lint"));
    let start = suggestions.iter().find(|s| s.text == "start").unwrap();
    assert_eq!(start.kind, SuggestionKind::ProviderValue);
    assert_eq!(start.source, SuggestionSource::Provider);
    assert_eq!(start.description.as_deref(), Some("node x.js"));
}

#[tokio::test]
async fn cargo_run_p_tab_lists_workspace_members() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        b"[workspace]\nmembers = [\"a\", \"b\"]\n",
    )
    .unwrap();
    for (rel, name) in [("a", "alpha"), ("b", "beta")] {
        let dir = tmp.path().join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
    }

    let engine = engine();
    let ctx = ctx_for(tmp.path());
    let suggestions = engine
        .resolve_providers(&[ProviderKind::CargoWorkspaceMembers], &ctx, "")
        .await
        .unwrap();
    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert!(texts.contains(&"alpha"));
    assert!(texts.contains(&"beta"));
}

#[tokio::test]
async fn cargo_p_in_single_package_crate_lists_one_name() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        b"[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let engine = engine();
    let ctx = ctx_for(tmp.path());
    let suggestions = engine
        .resolve_providers(&[ProviderKind::CargoWorkspaceMembers], &ctx, "")
        .await
        .unwrap();
    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["solo"]);
}

#[tokio::test]
async fn missing_files_yield_empty_suggestions_without_panic() {
    let tmp = TempDir::new().unwrap();
    let engine = engine();
    let ctx = ctx_for(tmp.path());
    for kind in [
        ProviderKind::MakefileTargets,
        ProviderKind::NpmScripts,
        ProviderKind::CargoWorkspaceMembers,
    ] {
        let result = engine.resolve_providers(&[kind], &ctx, "").await.unwrap();
        assert!(
            result.is_empty(),
            "missing project file for {kind:?} must yield empty result, got {result:?}"
        );
    }
}

#[tokio::test]
async fn makefile_targets_invalidate_when_file_changes() {
    // End-to-end coverage for the cache-invalidation path through
    // MakefileTargets specifically — the underlying MtimeCache has unit
    // tests with a counter, but a subtle bug between the cache and the
    // provider's `parse_makefile_targets` call could silently serve a
    // stale target list. This test pins the provider's actual output.
    let tmp = TempDir::new().unwrap();
    let mf = tmp.path().join("Makefile");
    std::fs::write(&mf, b"build:\n\ttouch x\n").unwrap();

    let engine = engine();
    let ctx = ctx_for(tmp.path());

    let first = engine
        .resolve_providers(&[ProviderKind::MakefileTargets], &ctx, "")
        .await
        .unwrap();
    let texts: Vec<&str> = first.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["build"]);

    std::fs::write(&mf, b"build:\n\ttouch x\ntest:\n\tcargo test\n").unwrap();
    // Bump mtime forward so the (mtime, size) probe definitely sees a
    // change even on filesystems with coarse mtime granularity.
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    let ft = filetime::FileTime::from_system_time(future);
    filetime::set_file_mtime(&mf, ft).unwrap();

    let second = engine
        .resolve_providers(&[ProviderKind::MakefileTargets], &ctx, "")
        .await
        .unwrap();
    let texts: Vec<&str> = second.iter().map(|s| s.text.as_str()).collect();
    assert!(texts.contains(&"build"));
    assert!(texts.contains(&"test"));
}

#[tokio::test]
async fn fuzzy_query_filters_provider_output() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Makefile"),
        b"build:\n\tcc\nbuild_release:\n\tcc\ntest:\n\tcargo test\n",
    )
    .unwrap();
    let engine = engine();
    let ctx = ctx_for(tmp.path());
    // Non-empty query routes through `fuzzy::rank` — only entries
    // matching `bui` should survive.
    let suggestions = engine
        .resolve_providers(&[ProviderKind::MakefileTargets], &ctx, "bui")
        .await
        .unwrap();
    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert!(texts.contains(&"build"));
    assert!(texts.contains(&"build_release"));
    assert!(!texts.contains(&"test"));
}
