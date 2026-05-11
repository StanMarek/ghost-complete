mod providers {
    pub use gc_suggest::providers::{Provider, ProviderCtx};
}

mod types {
    pub use gc_suggest::types::{Suggestion, SuggestionKind, SuggestionSource};
}

#[path = "../src/providers/npm_local.rs"]
mod npm_local;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use gc_suggest::providers::{local_project::npm_scripts::NpmScripts, Provider, ProviderCtx};
use gc_suggest::types::{SuggestionKind, SuggestionSource};
use npm_local::{
    parse_npm_dependencies, NpmAllDependencies, NpmDependencies, NpmDependencyMode,
    NpmDevDependencies,
};

fn ctx(cwd: &Path) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    }
}

const PACKAGE_JSON: &[u8] = br#"{
  "name": "demo",
  "scripts": {
    "build": "tsc",
    "test": "vitest"
  },
  "dependencies": {
    "react": "^19.0.0",
    "lodash": "^4.17.21"
  },
  "devDependencies": {
    "vitest": "^3.0.0",
    "react": "^19.0.0"
  }
}"#;

#[test]
fn parses_dependency_sections_and_all_union_in_source_order() {
    let deps = parse_npm_dependencies(PACKAGE_JSON, NpmDependencyMode::Dependencies);
    assert_eq!(
        deps.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["react", "lodash"]
    );
    assert_eq!(deps[0].description.as_deref(), Some("^19.0.0"));
    assert_eq!(deps[1].description.as_deref(), Some("^4.17.21"));

    let dev_deps = parse_npm_dependencies(PACKAGE_JSON, NpmDependencyMode::DevDependencies);
    assert_eq!(
        dev_deps.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["vitest", "react"]
    );

    let all = parse_npm_dependencies(PACKAGE_JSON, NpmDependencyMode::AllDependencies);
    assert_eq!(
        all.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["react", "lodash", "vitest"]
    );
    for suggestion in all {
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn malformed_and_missing_dependency_fields_return_empty() {
    assert!(parse_npm_dependencies(b"not json", NpmDependencyMode::Dependencies).is_empty());
    assert!(parse_npm_dependencies(b"{", NpmDependencyMode::DevDependencies).is_empty());
    assert!(
        parse_npm_dependencies(br#"{"name":"demo"}"#, NpmDependencyMode::AllDependencies)
            .is_empty()
    );
    assert!(parse_npm_dependencies(
        br#"{"dependencies":["react"],"devDependencies":"vitest"}"#,
        NpmDependencyMode::AllDependencies
    )
    .is_empty());
}

#[test]
fn non_string_dependency_value_does_not_block_later_valid_union_entry() {
    let json = br#"{
      "dependencies": {
        "shared": 42
      },
      "devDependencies": {
        "shared": "^1.0.0",
        "vitest": "^3.0.0"
      }
    }"#;

    let all = parse_npm_dependencies(json, NpmDependencyMode::AllDependencies);

    assert_eq!(
        all.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["shared", "vitest"]
    );
    assert_eq!(all[0].description.as_deref(), Some("^1.0.0"));
}

#[tokio::test]
async fn generate_walks_to_nearest_package_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("package.json"), PACKAGE_JSON).unwrap();
    let nested = tmp.path().join("packages/web/src");
    std::fs::create_dir_all(&nested).unwrap();

    let deps = NpmDependencies.generate_with_root(&nested).await.unwrap();
    assert_eq!(
        deps.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["react", "lodash"]
    );

    let dev_deps = NpmDevDependencies
        .generate_with_root(&nested)
        .await
        .unwrap();
    assert_eq!(
        dev_deps.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["vitest", "react"]
    );

    let all = NpmAllDependencies
        .generate_with_root(&nested)
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["react", "lodash", "vitest"]
    );
}

#[tokio::test]
async fn missing_package_json_returns_ok_empty() {
    let tmp = tempfile::TempDir::new().unwrap();

    let result = NpmAllDependencies.generate_with_root(tmp.path()).await;

    assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
}

#[tokio::test]
async fn malformed_package_json_returns_ok_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("package.json"), b"not json").unwrap();

    let result = NpmDependencies.generate_with_root(tmp.path()).await;

    assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
}

#[tokio::test]
async fn dependency_lookup_matches_npm_scripts_nearest_package_shape() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("package.json"), PACKAGE_JSON).unwrap();
    let nested = tmp.path().join("packages/web/src");
    std::fs::create_dir_all(&nested).unwrap();

    let deps = NpmAllDependencies.generate(&ctx(&nested)).await.unwrap();
    let scripts = NpmScripts.generate(&ctx(&nested)).await.unwrap();

    assert_eq!(
        deps.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["react", "lodash", "vitest"]
    );
    assert_eq!(
        scripts.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["build", "test"]
    );
}
