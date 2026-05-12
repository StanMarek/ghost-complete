use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::time::timeout;

use crate::aws::clients::AWS_CLIENTS;
use crate::aws::error::AwsProviderError;
use crate::cache::{CacheKey, GeneratorCache};
use crate::json_path::JsonPath;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

use super::{Provider, ProviderCtx};

const AWS_SDK_TIMEOUT: Duration = Duration::from_millis(800);
const AWS_SDK_CACHE_TTL: Duration = Duration::from_secs(60);

static AWS_SDK_SUGGESTION_CACHE: LazyLock<GeneratorCache> = LazyLock::new(GeneratorCache::new);

pub(crate) trait AwsOperationRuntime: Send + Sync {
    fn call<'a>(
        &'a self,
        service: &'a str,
        operation: &'a str,
        ctx: &'a ProviderCtx,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AwsProviderError>> + Send + 'a>>;
}

struct LiveAwsRuntime;

impl AwsOperationRuntime for LiveAwsRuntime {
    fn call<'a>(
        &'a self,
        service: &'a str,
        operation: &'a str,
        ctx: &'a ProviderCtx,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AwsProviderError>> + Send + 'a>> {
        Box::pin(async move {
            match service {
                "iam" => {
                    AWS_CLIENTS
                        .call_iam(
                            operation,
                            &ctx.params,
                            aws_profile(ctx).as_deref(),
                            aws_region(ctx).as_deref(),
                        )
                        .await
                }
                _ => Ok(Value::Null),
            }
        })
    }
}

pub struct AwsSdk;

impl AwsSdk {
    async fn generate_with_runtime<R>(
        &self,
        ctx: &ProviderCtx,
        runtime: &R,
        call_timeout: Duration,
    ) -> Result<Vec<Suggestion>>
    where
        R: AwsOperationRuntime,
    {
        let Some(service) = ctx.params.get("service").map(String::as_str) else {
            return Ok(Vec::new());
        };
        let Some(operation) = ctx.params.get("operation").map(String::as_str) else {
            return Ok(Vec::new());
        };
        if !is_supported_operation(service, operation) {
            return Ok(Vec::new());
        }

        let response = match timeout(call_timeout, runtime.call(service, operation, ctx)).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                tracing::warn!(
                    service,
                    operation,
                    kind = ?error.kind,
                    error = %error,
                    "aws sdk provider call failed"
                );
                use crate::aws::error::AwsSdkErrorKind;
                return match error.kind {
                    AwsSdkErrorKind::AuthMissing | AwsSdkErrorKind::AuthExpired => {
                        Err(anyhow::anyhow!(
                            "AWS credentials unavailable or expired; run `aws sso login` or configure AWS credentials"
                        ))
                    }
                    AwsSdkErrorKind::Network => Err(anyhow::anyhow!(
                        "AWS API unreachable ({service} {operation}); check network connectivity"
                    )),
                    AwsSdkErrorKind::Other => Err(anyhow::anyhow!(
                        "AWS {service} {operation} failed: {}",
                        error.message
                    )),
                };
            }
            Err(_) => {
                tracing::warn!(service, operation, "aws sdk provider call timed out");
                return Ok(Vec::new());
            }
        };

        suggestions_from_response_json(ctx, &response)
    }
}

impl Provider for AwsSdk {
    fn name(&self) -> &'static str {
        "aws_sdk"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        let Some(service) = ctx.params.get("service").map(String::as_str) else {
            return Ok(Vec::new());
        };
        let Some(operation) = ctx.params.get("operation").map(String::as_str) else {
            return Ok(Vec::new());
        };
        if !is_supported_operation(service, operation) {
            return Ok(Vec::new());
        }

        let key = aws_cache_key(ctx, service, operation);
        AWS_SDK_SUGGESTION_CACHE
            .get_or_try_insert_with(key, AWS_SDK_CACHE_TTL, || async {
                self.generate_with_runtime(ctx, &LiveAwsRuntime, AWS_SDK_TIMEOUT)
                    .await
            })
            .await
    }
}

fn aws_cache_key(ctx: &ProviderCtx, service: &str, operation: &str) -> CacheKey {
    CacheKey::AwsSdk {
        service: service.to_string(),
        operation: operation.to_string(),
        params_hash: ctx.params_hash(),
        profile: aws_profile(ctx),
        region: aws_region(ctx),
    }
}

fn aws_profile(ctx: &ProviderCtx) -> Option<String> {
    param_or_env(ctx, "profile", &["AWS_PROFILE", "AWS_DEFAULT_PROFILE"])
}

fn aws_region(ctx: &ProviderCtx) -> Option<String> {
    param_or_env(ctx, "region", &["AWS_REGION", "AWS_DEFAULT_REGION"])
}

fn param_or_env(ctx: &ProviderCtx, param: &str, env_names: &[&str]) -> Option<String> {
    ctx.params
        .get(param)
        .and_then(|value| clean(value).map(str::to_string))
        .or_else(|| {
            env_names
                .iter()
                .find_map(|name| ctx.env(name).and_then(clean).map(str::to_string))
        })
}

pub fn suggestions_from_response_json(
    ctx: &ProviderCtx,
    response: &Value,
) -> Result<Vec<Suggestion>> {
    let Some(field) = ctx.params.get("field") else {
        return Ok(Vec::new());
    };
    let field_path = match JsonPath::parse(field) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(field, error, "aws sdk provider field path is invalid");
            return Ok(Vec::new());
        }
    };
    let description_path =
        ctx.params
            .get("description_field")
            .and_then(|field| match JsonPath::parse(field) {
                Ok(path) => Some(path),
                Err(error) => {
                    tracing::warn!(field, error, "aws sdk provider description path is invalid");
                    None
                }
            });

    let names = field_path.lookup_each(response);
    let descriptions = description_path
        .as_ref()
        .map(|path| path.lookup_each(response))
        .unwrap_or_default();

    Ok(names
        .into_iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let text = value_to_string(value)?;
            let description = descriptions
                .get(index)
                .and_then(|value| value_to_string(value));
            Some(provider_suggestion(text, description))
        })
        .collect())
}

fn is_supported_operation(service: &str, operation: &str) -> bool {
    matches!(
        (service, operation),
        ("iam", "ListRoles")
            | ("iam", "ListPolicies")
            | ("iam", "ListUsers")
            | ("iam", "ListGroups")
    )
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => clean(value).map(str::to_string),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn clean(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn provider_suggestion(text: String, description: Option<String>) -> Suggestion {
    Suggestion {
        text,
        description,
        kind: SuggestionKind::ProviderValue,
        source: SuggestionSource::Provider,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    struct StaticRuntime {
        response: Value,
    }

    impl AwsOperationRuntime for StaticRuntime {
        fn call<'a>(
            &'a self,
            _service: &'a str,
            _operation: &'a str,
            _ctx: &'a ProviderCtx,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AwsProviderError>> + Send + 'a>> {
            Box::pin(async move { Ok(self.response.clone()) })
        }
    }

    struct ErrorRuntime {
        error: AwsProviderError,
    }

    impl AwsOperationRuntime for ErrorRuntime {
        fn call<'a>(
            &'a self,
            _service: &'a str,
            _operation: &'a str,
            _ctx: &'a ProviderCtx,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AwsProviderError>> + Send + 'a>> {
            Box::pin(async move {
                Err(AwsProviderError {
                    kind: self.error.kind,
                    message: self.error.message.clone(),
                })
            })
        }
    }

    fn ctx(params: impl IntoIterator<Item = (&'static str, &'static str)>) -> ProviderCtx {
        ProviderCtx {
            cwd: "/tmp".into(),
            env: Arc::new(Default::default()),
            current_token: String::new(),
            params: Arc::new(
                params
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ),
        }
    }

    #[tokio::test]
    async fn generate_uses_runtime_seam_for_supported_operation() {
        let runtime = StaticRuntime {
            response: json!({
                "Users": [{"UserName": "alice", "Arn": "arn:aws:iam::123:user/alice"}]
            }),
        };
        let ctx = ctx([
            ("service", "iam"),
            ("operation", "ListUsers"),
            ("field", "Users[*].UserName"),
            ("description_field", "Users[*].Arn"),
        ]);

        let suggestions = AwsSdk
            .generate_with_runtime(&ctx, &runtime, Duration::from_millis(10))
            .await
            .unwrap();

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "alice");
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("arn:aws:iam::123:user/alice")
        );
    }

    #[test]
    fn aws_cache_key_includes_profile_region_and_params_hash() {
        let env = Arc::new(std::collections::HashMap::from([
            ("AWS_PROFILE".to_string(), "env-profile".to_string()),
            ("AWS_REGION".to_string(), "us-east-1".to_string()),
        ]));
        let params = Arc::new(BTreeMap::from([
            ("field".to_string(), "Users[*].UserName".to_string()),
            ("operation".to_string(), "ListUsers".to_string()),
            ("profile".to_string(), "param-profile".to_string()),
            ("region".to_string(), "eu-west-1".to_string()),
            ("service".to_string(), "iam".to_string()),
        ]));
        let ctx = ProviderCtx {
            cwd: "/tmp".into(),
            env,
            current_token: String::new(),
            params,
        };

        assert_eq!(
            aws_cache_key(&ctx, "iam", "ListUsers"),
            CacheKey::AwsSdk {
                service: "iam".to_string(),
                operation: "ListUsers".to_string(),
                params_hash: ctx.params_hash(),
                profile: Some("param-profile".to_string()),
                region: Some("eu-west-1".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn generate_surfaces_auth_errors_for_ui_diagnostics() {
        let runtime = ErrorRuntime {
            error: AwsProviderError {
                kind: crate::aws::error::AwsSdkErrorKind::AuthMissing,
                message: "no credentials".to_string(),
            },
        };
        let ctx = ctx([
            ("service", "iam"),
            ("operation", "ListUsers"),
            ("field", "Users[*].UserName"),
        ]);

        let err = AwsSdk
            .generate_with_runtime(&ctx, &runtime, Duration::from_millis(10))
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("AWS credentials"),
            "auth failures should become user-visible provider diagnostics: {err}"
        );
    }

    #[tokio::test]
    async fn generate_surfaces_network_errors_for_ui_diagnostics() {
        let runtime = ErrorRuntime {
            error: AwsProviderError {
                kind: crate::aws::error::AwsSdkErrorKind::Network,
                message: "dispatch failure".to_string(),
            },
        };
        let ctx = ctx([
            ("service", "iam"),
            ("operation", "ListUsers"),
            ("field", "Users[*].UserName"),
        ]);

        let err = AwsSdk
            .generate_with_runtime(&ctx, &runtime, Duration::from_millis(10))
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("unreachable"),
            "network failures should reach the dynamic-feedback channel: {err}"
        );
    }

    #[tokio::test]
    async fn generate_surfaces_other_errors_for_ui_diagnostics() {
        let runtime = ErrorRuntime {
            error: AwsProviderError {
                kind: crate::aws::error::AwsSdkErrorKind::Other,
                message: "AccessDeniedException: not authorized".to_string(),
            },
        };
        let ctx = ctx([
            ("service", "iam"),
            ("operation", "ListUsers"),
            ("field", "Users[*].UserName"),
        ]);

        let err = AwsSdk
            .generate_with_runtime(&ctx, &runtime, Duration::from_millis(10))
            .await
            .unwrap_err();

        let rendered = err.to_string();
        assert!(
            rendered.contains("ListUsers") && rendered.contains("AccessDeniedException"),
            "other failures should include the operation and underlying message: {rendered}"
        );
    }
}
