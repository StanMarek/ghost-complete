mod providers {
    pub use gc_suggest::providers::{Provider, ProviderCtx};
}

mod types {
    pub use gc_suggest::types::{Suggestion, SuggestionKind, SuggestionSource};
}

#[path = "../src/providers/cargo_metadata.rs"]
mod cargo_metadata;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use cargo_metadata::{
    parse_cargo_features, parse_cargo_targets, parse_cargo_workspace_members, CargoFeatures,
    CargoTargets, CargoWorkspaceMembers,
};
use gc_suggest::providers::ProviderCtx;
use gc_suggest::types::{SuggestionKind, SuggestionSource};

fn ctx(cwd: &Path) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    }
}

fn ctx_with_kind(cwd: &Path, kind: &str) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::from([("kind".to_string(), kind.to_string())])),
    }
}

fn metadata_fixture(root: &Path) -> String {
    let api_manifest = root.join("crates/api/Cargo.toml");
    let core_manifest = root.join("crates/core/Cargo.toml");
    let xtask_manifest = root.join("xtask/Cargo.toml");
    format!(
        r#"{{
  "packages": [
    {{
      "id": "path+file://{core}#core-lib@0.1.0",
      "name": "core-lib",
      "manifest_path": "{core}",
      "features": {{
        "serde": [],
        "tracing": []
      }},
      "targets": [
        {{"name": "core_lib", "kind": ["lib"], "crate_types": ["lib"]}}
      ]
    }},
    {{
      "id": "path+file://{api}#api@0.1.0",
      "name": "api",
      "manifest_path": "{api}",
      "features": {{
        "default": ["tls"],
        "tls": [],
        "unstable": []
      }},
      "targets": [
        {{"name": "api", "kind": ["bin"], "crate_types": ["bin"]}},
        {{"name": "api-smoke", "kind": ["test"], "crate_types": ["bin"]}},
        {{"name": "demo", "kind": ["example"], "crate_types": ["bin"]}}
      ]
    }},
    {{
      "id": "path+file://{xtask}#xtask@0.1.0",
      "name": "xtask",
      "manifest_path": "{xtask}",
      "features": {{}},
      "targets": [
        {{"name": "xtask", "kind": ["bin"], "crate_types": ["bin"]}},
        {{"name": "load", "kind": ["bench"], "crate_types": ["bin"]}}
      ]
    }}
  ],
  "workspace_members": [
    "path+file://{core}#core-lib@0.1.0",
    "path+file://{api}#api@0.1.0",
    "path+file://{xtask}#xtask@0.1.0"
  ],
  "workspace_root": "{root}",
  "target_directory": "{root}/target",
  "version": 1
}}"#,
        api = api_manifest.display(),
        core = core_manifest.display(),
        xtask = xtask_manifest.display(),
        root = root.display()
    )
}

#[test]
fn parses_workspace_members_in_metadata_order() {
    let tmp = tempfile::TempDir::new().unwrap();
    let members = parse_cargo_workspace_members(metadata_fixture(tmp.path()).as_bytes());

    assert_eq!(members.len(), 3);
    assert_eq!(members[0].text, "core-lib");
    assert_eq!(members[1].text, "api");
    assert_eq!(members[2].text, "xtask");
    for suggestion in members {
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn parses_targets_filtered_by_kind_across_workspace_members() {
    let tmp = tempfile::TempDir::new().unwrap();
    let metadata = metadata_fixture(tmp.path());

    let bins = parse_cargo_targets(metadata.as_bytes(), "bin");
    assert_eq!(
        bins.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["api", "xtask"]
    );
    assert_eq!(bins[0].description.as_deref(), Some("api"));
    assert_eq!(bins[1].description.as_deref(), Some("xtask"));

    let examples = parse_cargo_targets(metadata.as_bytes(), "example");
    assert_eq!(
        examples.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["demo"]
    );

    let tests = parse_cargo_targets(metadata.as_bytes(), "test");
    assert_eq!(
        tests.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["api-smoke"]
    );

    let benches = parse_cargo_targets(metadata.as_bytes(), "bench");
    assert_eq!(
        benches.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["load"]
    );
}

#[test]
fn parses_features_for_nearest_manifest_package() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().join("crates/api/src");
    let features = parse_cargo_features(metadata_fixture(tmp.path()).as_bytes(), &cwd);

    assert_eq!(
        features.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["default", "tls", "unstable"]
    );
}

#[test]
fn malformed_metadata_returns_empty_suggestions() {
    let tmp = tempfile::TempDir::new().unwrap();

    assert!(parse_cargo_workspace_members(b"not json").is_empty());
    assert!(parse_cargo_targets(b"{", "bin").is_empty());
    assert!(parse_cargo_features(b"[]", tmp.path()).is_empty());
}

#[tokio::test]
async fn missing_cargo_binary_returns_ok_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = CargoTargets
        .generate_with_binary(
            &ctx_with_kind(tmp.path(), "bin"),
            "/nonexistent/cargo-for-test",
        )
        .await;

    assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
}

#[tokio::test]
async fn malformed_cargo_metadata_stdout_returns_ok_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cargo = tmp.path().join("fake-cargo");
    std::fs::write(&cargo, "#!/bin/sh\nprintf 'not json'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let result = CargoFeatures
        .generate_with_binary(&ctx(tmp.path()), cargo.to_str().unwrap())
        .await;

    assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
}

#[tokio::test]
async fn workspace_members_provider_uses_metadata_stdout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cargo = tmp.path().join("fake-cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\ncat <<'JSON'\n{}\nJSON\n",
            metadata_fixture(tmp.path())
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let suggestions = CargoWorkspaceMembers
        .generate_with_binary(&ctx(tmp.path()), cargo.to_str().unwrap())
        .await
        .unwrap();

    assert_eq!(
        suggestions
            .iter()
            .map(|suggestion| suggestion.text.as_str())
            .collect::<Vec<_>>(),
        vec!["core-lib", "api", "xtask"]
    );
}
