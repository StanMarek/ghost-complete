use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

mod types {
    pub use gc_suggest::types::*;
}

mod providers {
    pub use gc_suggest::providers::{Provider, ProviderCtx};

    pub mod util {
        include!("../src/providers/util.rs");
    }

    pub mod dscl_principals {
        include!("../src/providers/dscl_principals.rs");
    }
}

use gc_suggest::providers::{Provider, ProviderCtx};
use gc_suggest::types::{SuggestionKind, SuggestionSource};
use providers::dscl_principals::{
    include_system_from_ctx, parse_principals_output, run_dscl_list_with_binary, DsclGroups,
    DsclUsers,
};

fn ctx_with_params(cwd: &Path, params: &[(&str, &str)]) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(
            params
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
        ),
    }
}

#[test]
fn provider_names_match_spec_type_strings() {
    assert_eq!(DsclUsers.name(), "dscl_users");
    assert_eq!(DsclGroups.name(), "dscl_groups");
}

#[test]
fn users_parse_recorded_dscl_output_and_filter_system_by_default() {
    let fixture = "\
_analyticsd
_spotlight
daemon
nobody
root
stan
";
    let suggestions = parse_principals_output(fixture, false, "dscl user");

    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["daemon", "nobody", "root", "stan"]);
    for suggestion in &suggestions {
        assert_eq!(suggestion.description.as_deref(), Some("dscl user"));
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn groups_parse_recorded_dscl_output_and_filter_system_by_default() {
    let fixture = "\
_developer
_networkd
admin
staff
wheel
";
    let suggestions = parse_principals_output(fixture, false, "dscl group");

    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["admin", "staff", "wheel"]);
    for suggestion in &suggestions {
        assert_eq!(suggestion.description.as_deref(), Some("dscl group"));
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn include_system_keeps_underscore_prefixed_principals() {
    let fixture = "_analyticsd\nstan\n_spotlight\n";
    let suggestions = parse_principals_output(fixture, true, "dscl user");

    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["_analyticsd", "stan", "_spotlight"]);
}

#[test]
fn include_system_param_defaults_false_and_accepts_true() {
    let tmp = tempfile::TempDir::new().unwrap();
    let default_ctx = ctx_with_params(tmp.path(), &[]);
    let true_ctx = ctx_with_params(tmp.path(), &[("include_system", "true")]);
    let one_ctx = ctx_with_params(tmp.path(), &[("include_system", "1")]);
    let false_ctx = ctx_with_params(tmp.path(), &[("include_system", "false")]);

    assert!(!include_system_from_ctx(&default_ctx));
    assert!(include_system_from_ctx(&true_ctx));
    assert!(include_system_from_ctx(&one_ctx));
    assert!(!include_system_from_ctx(&false_ctx));
}

#[test]
fn parser_returns_empty_for_empty_output() {
    assert!(parse_principals_output("", false, "dscl user").is_empty());
    assert!(parse_principals_output("\n\n", false, "dscl group").is_empty());
}

#[tokio::test]
async fn run_dscl_missing_binary_returns_none() {
    let tmp = tempfile::TempDir::new().unwrap();

    let output = run_dscl_list_with_binary(
        tmp.path(),
        "/nonexistent/dscl-definitely-not-real",
        "/Users",
    )
    .await;

    assert!(output.is_none());
}

#[tokio::test]
async fn providers_return_ok_empty_when_dscl_binary_is_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = ctx_with_params(tmp.path(), &[]);
    let missing = "/nonexistent/dscl-for-provider-tests";

    let users = DsclUsers.generate_with_binary(&ctx, missing).await.unwrap();
    let groups = DsclGroups
        .generate_with_binary(&ctx, missing)
        .await
        .unwrap();

    assert!(users.is_empty());
    assert!(groups.is_empty());
}
