// macOS Directory Service principal providers for users and groups.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use super::util::{is_binary_missing, spawn_with_timeout};
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
        Err(error) if is_binary_missing(&error) => {
            tracing::trace!(binary, "dscl binary not installed");
            None
        }
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

/// Decoded shape of a partially-typed chown owner/group token.
///
/// `chown` accepts `OWNER`, `OWNER:GROUP`, or `:GROUP` as a single
/// argument. The provider emits pre-prefixed completions (e.g.
/// `stan:staff` for the typed token `stan:`) carrying the full owner
/// and colon, rather than relying on the engine to reconstruct the
/// `owner:` prefix from a bare group name. At merge time the engine
/// re-ranks the merged pool with `gc_suggest::fuzzy::rank` against the
/// live `current_word` (the colon-containing token) — nucleo scores
/// `:` as an ordinary character in a subsequence match, so a
/// pre-prefixed `stan:staff` candidate still matches the query
/// `stan:` and survives the re-rank.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChownToken<'a> {
    /// No colon typed yet. `prefix` is what's been typed so far (may
    /// be empty when the cursor sits at a word boundary).
    OwnerOnly { prefix: &'a str },
    /// Token starts with `:`. The user wants only a group; preserve
    /// the leading colon in emitted text.
    GroupOnly { prefix: &'a str },
    /// `owner:` or `owner:groupPrefix` — owner is locked in and the
    /// remaining completion is the group.
    OwnerGroup {
        owner: &'a str,
        group_prefix: &'a str,
    },
}

pub(crate) fn classify_chown_token(token: &str) -> ChownToken<'_> {
    if let Some(rest) = token.strip_prefix(':') {
        return ChownToken::GroupOnly { prefix: rest };
    }
    if let Some((owner, group)) = token.split_once(':') {
        return ChownToken::OwnerGroup {
            owner,
            group_prefix: group,
        };
    }
    ChownToken::OwnerOnly { prefix: token }
}

/// Build `chown_owner_group` suggestions from already-fetched principal
/// lists. The colon-emission rules live here so they can be unit-tested
/// without spawning `dscl`.
///
/// The `users` and `groups` slices are expected to already honour the
/// caller's `include_system` decision — `parse_principals_output` is
/// the right preprocessor.
pub(crate) fn chown_owner_group_from_principals(
    token: &str,
    users: &[String],
    groups: &[String],
) -> Vec<Suggestion> {
    match classify_chown_token(token) {
        ChownToken::OwnerOnly { prefix } => users
            .iter()
            .filter(|u| u.starts_with(prefix))
            .map(|u| Suggestion {
                text: u.clone(),
                description: Some("user".into()),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
            .collect(),
        ChownToken::GroupOnly { prefix } => groups
            .iter()
            .filter(|g| g.starts_with(prefix))
            .map(|g| Suggestion {
                text: format!(":{g}"),
                description: Some("group".into()),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
            .collect(),
        ChownToken::OwnerGroup {
            owner,
            group_prefix,
        } => groups
            .iter()
            .filter(|g| g.starts_with(group_prefix))
            .map(|g| Suggestion {
                text: format!("{owner}:{g}"),
                description: Some(format!("user {owner}, group {g}")),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
            .collect(),
    }
}

/// `chown_owner_group` — colon-aware completion for chown's first
/// positional argument. Replaces the legacy `[dscl_users, dscl_groups]`
/// pair, which couldn't account for the `OWNER:GROUP` form because
/// nucleo treats `:` as a fuzzy delimiter.
///
/// The dispatch fetches ONLY the principal set the token shape needs —
/// `/Users` for an owner-only token, `/Groups` for a `:group` or
/// `owner:group` token — via mutually exclusive branches, so each
/// completion spawns at most one `dscl` call (never both). The pure
/// [`chown_owner_group_from_principals`] then formats the surfaced set
/// based on the same token shape.
pub struct ChownOwnerGroup;

impl Provider for ChownOwnerGroup {
    fn name(&self) -> &'static str {
        "chown_owner_group"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "dscl").await
    }
}

impl ChownOwnerGroup {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let include_system = include_system_from_ctx(ctx);
        let token = ctx.current_token.as_str();
        let want_groups = matches!(
            classify_chown_token(token),
            ChownToken::GroupOnly { .. } | ChownToken::OwnerGroup { .. }
        );

        let users = if matches!(classify_chown_token(token), ChownToken::OwnerOnly { .. }) {
            match run_dscl_list_with_binary(&ctx.cwd, binary, "/Users").await {
                Some(output) => parse_principals_output(&output, include_system, "user")
                    .into_iter()
                    .map(|s| s.text)
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };

        let groups = if want_groups {
            match run_dscl_list_with_binary(&ctx.cwd, binary, "/Groups").await {
                Some(output) => parse_principals_output(&output, include_system, "group")
                    .into_iter()
                    .map(|s| s.text)
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };

        Ok(chown_owner_group_from_principals(token, &users, &groups))
    }
}
