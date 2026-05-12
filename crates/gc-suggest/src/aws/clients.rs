use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use aws_config::BehaviorVersion;
use aws_sdk_iam::types::PolicyScopeType;
use serde_json::{json, Value};

use super::error::{classify_sdk_error, AwsProviderError};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct AwsClientKey {
    profile: Option<String>,
    region: Option<String>,
}

pub(crate) struct AwsClients {
    iam: Mutex<HashMap<AwsClientKey, aws_sdk_iam::Client>>,
}

impl AwsClients {
    fn new() -> Self {
        Self {
            iam: Mutex::new(HashMap::new()),
        }
    }

    async fn config_for(profile: Option<&str>, region: Option<&str>) -> aws_config::SdkConfig {
        if std::env::var_os("AWS_EC2_METADATA_DISABLED").is_none() {
            std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
        }

        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if let Some(profile) = profile {
            loader = loader.profile_name(profile.to_string());
        }
        if let Some(region) = region {
            loader = loader.region(aws_sdk_iam::config::Region::new(region.to_string()));
        }
        loader.load().await
    }

    async fn iam(&self, profile: Option<&str>, region: Option<&str>) -> aws_sdk_iam::Client {
        let key = AwsClientKey {
            profile: profile.map(str::to_string),
            region: region.map(str::to_string),
        };

        {
            let guard = self.iam.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(client) = guard.get(&key) {
                return client.clone();
            }
        }

        let config = Self::config_for(profile, region).await;
        let client = aws_sdk_iam::Client::new(&config);
        let mut guard = self.iam.lock().unwrap_or_else(|e| e.into_inner());
        guard.entry(key).or_insert_with(|| client.clone()).clone()
    }

    pub(crate) async fn call_iam(
        &self,
        operation: &str,
        params: &BTreeMap<String, String>,
        profile: Option<&str>,
        region: Option<&str>,
    ) -> Result<Value, AwsProviderError> {
        match operation {
            "ListRoles" => self.list_roles(profile, region).await,
            "ListPolicies" => self.list_policies(params, profile, region).await,
            "ListUsers" => self.list_users(profile, region).await,
            "ListGroups" => self.list_groups(profile, region).await,
            _ => Ok(Value::Null),
        }
    }

    async fn list_roles(
        &self,
        profile: Option<&str>,
        region: Option<&str>,
    ) -> Result<Value, AwsProviderError> {
        let roles = self
            .iam(profile, region)
            .await
            .list_roles()
            .into_paginator()
            .items()
            .send()
            .try_collect()
            .await
            .map_err(|error| AwsProviderError {
                kind: classify_sdk_error(&error),
                message: error.to_string(),
            })?;
        Ok(json!({
            "Roles": roles
                .iter()
                .map(|role| {
                    json!({
                        "RoleName": role.role_name(),
                        "Arn": role.arn(),
                    })
                })
                .collect::<Vec<_>>()
        }))
    }

    async fn list_policies(
        &self,
        params: &BTreeMap<String, String>,
        profile: Option<&str>,
        region: Option<&str>,
    ) -> Result<Value, AwsProviderError> {
        let mut request = self.iam(profile, region).await.list_policies();
        if params
            .get("Scope")
            .is_some_and(|scope| scope.eq_ignore_ascii_case("Local"))
        {
            request = request.scope(PolicyScopeType::Local);
        }
        let policies = request
            .into_paginator()
            .items()
            .send()
            .try_collect()
            .await
            .map_err(|error| AwsProviderError {
                kind: classify_sdk_error(&error),
                message: error.to_string(),
            })?;
        Ok(json!({
            "Policies": policies
                .iter()
                .map(|policy| {
                    json!({
                        "PolicyName": policy.policy_name(),
                        "Arn": policy.arn(),
                    })
                })
                .collect::<Vec<_>>()
        }))
    }

    async fn list_users(
        &self,
        profile: Option<&str>,
        region: Option<&str>,
    ) -> Result<Value, AwsProviderError> {
        let users = self
            .iam(profile, region)
            .await
            .list_users()
            .into_paginator()
            .items()
            .send()
            .try_collect()
            .await
            .map_err(|error| AwsProviderError {
                kind: classify_sdk_error(&error),
                message: error.to_string(),
            })?;
        Ok(json!({
            "Users": users
                .iter()
                .map(|user| {
                    json!({
                        "UserName": user.user_name(),
                        "Arn": user.arn(),
                    })
                })
                .collect::<Vec<_>>()
        }))
    }

    async fn list_groups(
        &self,
        profile: Option<&str>,
        region: Option<&str>,
    ) -> Result<Value, AwsProviderError> {
        let groups = self
            .iam(profile, region)
            .await
            .list_groups()
            .into_paginator()
            .items()
            .send()
            .try_collect()
            .await
            .map_err(|error| AwsProviderError {
                kind: classify_sdk_error(&error),
                message: error.to_string(),
            })?;
        Ok(json!({
            "Groups": groups
                .iter()
                .map(|group| {
                    json!({
                        "GroupName": group.group_name(),
                        "Arn": group.arn(),
                    })
                })
                .collect::<Vec<_>>()
        }))
    }
}

pub(crate) static AWS_CLIENTS: LazyLock<AwsClients> = LazyLock::new(AwsClients::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_key_separates_profile_and_region() {
        assert_ne!(
            AwsClientKey {
                profile: Some("dev".to_string()),
                region: Some("us-east-1".to_string()),
            },
            AwsClientKey {
                profile: Some("prod".to_string()),
                region: Some("us-east-1".to_string()),
            }
        );
        assert_ne!(
            AwsClientKey {
                profile: Some("dev".to_string()),
                region: Some("us-east-1".to_string()),
            },
            AwsClientKey {
                profile: Some("dev".to_string()),
                region: Some("eu-west-1".to_string()),
            }
        );
    }
}
