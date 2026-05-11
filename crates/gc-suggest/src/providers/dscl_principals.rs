// macOS Directory Service principal providers for users and groups.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use super::util::spawn_with_timeout;
use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const DSCL_TIMEOUT_MS: u64 = 2_000;

pub(crate) async fn run_dscl_list_with_binary(
    cwd: &Path,
    binary: &str,
    node: &str,
) -> Option<String> {
    match spawn_with_timeout(
        cwd,
        binary,
        [".", "list", node],
        None,
        Duration::from_millis(DSCL_TIMEOUT_MS),
    )
    .await
    {
        Ok(stdout) => Some(stdout),
        Err(error) => {
            tracing::warn!(binary, error = %error, "dscl list command failed");
            None
        }
    }
}

pub(crate) fn include_system_from_ctx(ctx: &ProviderCtx) -> bool {
    ctx.params
        .get("include_system")
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

pub(crate) fn parse_principals_output(
    text: &str,
    include_system: bool,
    description: &'static str,
) -> Vec<Suggestion> {
    text.lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter(|name| include_system || !name.starts_with('_'))
        .map(|name| Suggestion {
            text: name.to_string(),
            description: Some(description.to_string()),
            kind: SuggestionKind::ProviderValue,
            source: SuggestionSource::Provider,
            ..Default::default()
        })
        .collect()
}

pub struct DsclUsers;

impl Provider for DsclUsers {
    fn name(&self) -> &'static str {
        "dscl_users"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "dscl").await
    }
}

impl DsclUsers {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(output) = run_dscl_list_with_binary(&ctx.cwd, binary, "/Users").await else {
            return Ok(Vec::new());
        };
        Ok(parse_principals_output(
            &output,
            include_system_from_ctx(ctx),
            "dscl user",
        ))
    }
}

pub struct DsclGroups;

impl Provider for DsclGroups {
    fn name(&self) -> &'static str {
        "dscl_groups"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "dscl").await
    }
}

impl DsclGroups {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(output) = run_dscl_list_with_binary(&ctx.cwd, binary, "/Groups").await else {
            return Ok(Vec::new());
        };
        Ok(parse_principals_output(
            &output,
            include_system_from_ctx(ctx),
            "dscl group",
        ))
    }
}
