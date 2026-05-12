use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use gc_suggest::providers::aws_profile_names::AwsProfileNames;
use gc_suggest::providers::aws_sdk::{suggestions_from_response_json, AwsSdk};
use gc_suggest::providers::{kind_from_type_str, Provider, ProviderCtx, ProviderKind};
use serde_json::json;

fn ctx_with_params(params: impl IntoIterator<Item = (&'static str, &'static str)>) -> ProviderCtx {
    ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(
            params
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<BTreeMap<_, _>>(),
        ),
    }
}

#[test]
fn provider_kind_recognizes_aws_providers() {
    assert_eq!(kind_from_type_str("aws_sdk"), Some(ProviderKind::AwsSdk));
    assert_eq!(
        kind_from_type_str("aws_profile_names"),
        Some(ProviderKind::AwsProfileNames)
    );
}

#[test]
fn aws_sdk_projects_iam_json_with_configured_fields() {
    let ctx = ctx_with_params([
        ("service", "iam"),
        ("operation", "ListRoles"),
        ("field", "Roles[*].RoleName"),
        ("description_field", "Roles[*].Arn"),
    ]);
    let response = json!({
        "Roles": [
            {"RoleName": "Admin", "Arn": "arn:aws:iam::123456789012:role/Admin"},
            {"RoleName": "ReadOnly", "Arn": "arn:aws:iam::123456789012:role/ReadOnly"}
        ]
    });

    let suggestions = suggestions_from_response_json(&ctx, &response).unwrap();

    let pairs: Vec<_> = suggestions
        .iter()
        .map(|suggestion| (suggestion.text.as_str(), suggestion.description.as_deref()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("Admin", Some("arn:aws:iam::123456789012:role/Admin")),
            ("ReadOnly", Some("arn:aws:iam::123456789012:role/ReadOnly")),
        ]
    );
}

#[tokio::test]
async fn aws_sdk_unsupported_operation_returns_empty_suggestions() {
    let provider = AwsSdk;
    let ctx = ctx_with_params([
        ("service", "iam"),
        ("operation", "DeleteRole"),
        ("field", "Roles[*].RoleName"),
    ]);

    let suggestions = provider.generate(&ctx).await.unwrap();

    assert!(suggestions.is_empty());
}

#[tokio::test]
async fn aws_profile_names_reads_config_and_credentials_files() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let credentials = temp.path().join("credentials");
    std::fs::write(
        &config,
        r#"
[default]
region = us-east-1

[profile dev]
region = us-west-2

[profile staging]
sso_session = corp
"#,
    )
    .unwrap();
    std::fs::write(
        &credentials,
        r#"
[default]
aws_access_key_id = test
aws_secret_access_key = test

[prod]
aws_access_key_id = test
aws_secret_access_key = test
"#,
    )
    .unwrap();

    let ctx = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::from([
            (
                "AWS_CONFIG_FILE".to_string(),
                config.to_string_lossy().into_owned(),
            ),
            (
                "AWS_SHARED_CREDENTIALS_FILE".to_string(),
                credentials.to_string_lossy().into_owned(),
            ),
        ])),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    };

    let suggestions = AwsProfileNames.generate(&ctx).await.unwrap();
    let names: Vec<_> = suggestions
        .iter()
        .map(|suggestion| suggestion.text.as_str())
        .collect();

    assert_eq!(names, vec!["default", "dev", "prod", "staging"]);
}
