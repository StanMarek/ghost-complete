use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use super::util::{is_binary_missing, parse_json_root, spawn_with_timeout};
use super::version_probe;
use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const SYSTEMCTL_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SystemdUnit {
    unit: String,
    active: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SystemdJsonUnit {
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    active: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemdFilter {
    All,
    ActiveOnly,
}

#[derive(Debug, Clone, Copy)]
enum SystemdScope {
    System,
    User,
}

impl SystemdScope {
    fn is_user(self) -> bool {
        matches!(self, Self::User)
    }
}

pub(crate) fn parse_systemd_json_units(stdout: &str) -> Option<Vec<SystemdUnit>> {
    let rows = parse_json_root::<Vec<SystemdJsonUnit>>(stdout).ok()?;
    Some(
        rows.into_iter()
            .filter_map(|row| {
                let unit = row.unit?.trim().to_string();
                if unit.is_empty() {
                    return None;
                }

                Some(SystemdUnit {
                    unit,
                    active: non_empty_or_unknown(row.active.as_deref()),
                    description: row
                        .description
                        .as_deref()
                        .map(str::trim)
                        .filter(|description| !description.is_empty())
                        .map(str::to_string),
                })
            })
            .collect(),
    )
}

pub(crate) fn parse_systemd_text_units(stdout: &str) -> Vec<SystemdUnit> {
    stdout.lines().filter_map(parse_systemd_text_line).collect()
}

fn parse_systemd_text_line(line: &str) -> Option<SystemdUnit> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.first().is_some_and(|part| is_status_marker(part)) {
        parts.remove(0);
    }
    if parts.len() < 4 {
        return None;
    }

    let unit = parts[0].trim();
    if unit.is_empty() || !unit.contains('.') {
        return None;
    }

    let active = non_empty_or_unknown(Some(parts[2]));
    let description = (parts.len() > 4).then(|| parts[4..].join(" "));
    Some(SystemdUnit {
        unit: unit.to_string(),
        active,
        description: description.filter(|description| !description.trim().is_empty()),
    })
}

fn is_status_marker(part: &str) -> bool {
    part == "*" || part == "\u{25cf}"
}

fn non_empty_or_unknown(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

pub(crate) fn systemd_units_to_suggestions(
    units: Vec<SystemdUnit>,
    filter: SystemdFilter,
) -> Vec<Suggestion> {
    let mut suggestions: Vec<Suggestion> = units
        .into_iter()
        .filter(|unit| match filter {
            SystemdFilter::All => true,
            SystemdFilter::ActiveOnly => unit.active.eq_ignore_ascii_case("active"),
        })
        .map(|unit| Suggestion {
            text: unit.unit,
            description: Some(systemd_description(
                &unit.active,
                unit.description.as_deref(),
            )),
            kind: SuggestionKind::ProviderValue,
            source: SuggestionSource::Provider,
            ..Default::default()
        })
        .collect();

    suggestions.sort_by(|a, b| a.text.cmp(&b.text));
    suggestions
}

fn systemd_description(active: &str, description: Option<&str>) -> String {
    let prefix = status_prefix(active);
    match description.map(str::trim).filter(|value| !value.is_empty()) {
        Some(description) => format!("{prefix} - {description}"),
        None => prefix,
    }
}

fn status_prefix(active: &str) -> String {
    match active {
        "active" => "active".to_string(),
        "failed" => "failed".to_string(),
        "reloading" => "reloading".to_string(),
        "activating" | "deactivating" => format!("pending {active}"),
        "" => "unknown".to_string(),
        other => other.to_string(),
    }
}

async fn run_systemctl_stdout(cwd: &Path, binary: &str, args: &[&str]) -> Option<String> {
    match spawn_with_timeout(
        cwd,
        binary,
        args.iter().copied(),
        None,
        Duration::from_millis(SYSTEMCTL_TIMEOUT_MS),
    )
    .await
    {
        Ok(stdout) => Some(stdout),
        Err(error) if is_binary_missing(&error) => {
            tracing::trace!(binary, "systemctl binary not installed");
            None
        }
        Err(error) => {
            tracing::warn!(binary, error = %error, "systemctl provider command failed");
            None
        }
    }
}

fn json_args(scope: SystemdScope) -> Vec<&'static str> {
    let mut args = Vec::with_capacity(7);
    if scope.is_user() {
        args.push("--user");
    }
    args.extend_from_slice(&["list-units", "-o", "json", "--all", "--full"]);
    args
}

fn text_args(scope: SystemdScope) -> Vec<&'static str> {
    let mut args = Vec::with_capacity(5);
    if scope.is_user() {
        args.push("--user");
    }
    args.extend_from_slice(&["list-units", "--no-legend", "--all", "--full"]);
    args
}

async fn run_systemctl_units(
    cwd: &Path,
    binary: &str,
    scope: SystemdScope,
) -> Option<Vec<SystemdUnit>> {
    let json_supported = !matches!(
        version_probe::probe_version(binary, "247").await,
        Some(false)
    );
    if json_supported {
        if let Some(stdout) = run_systemctl_stdout(cwd, binary, &json_args(scope)).await {
            if let Some(units) = parse_systemd_json_units(&stdout) {
                return Some(units);
            }
            tracing::warn!(
                binary,
                "systemctl JSON output parse failed; trying text fallback"
            );
        }
    }

    run_systemctl_stdout(cwd, binary, &text_args(scope))
        .await
        .map(|stdout| parse_systemd_text_units(&stdout))
}

async fn generate_systemd(
    ctx: &ProviderCtx,
    binary: &str,
    scope: SystemdScope,
    filter: SystemdFilter,
) -> Result<Vec<Suggestion>> {
    let Some(units) = run_systemctl_units(&ctx.cwd, binary, scope).await else {
        return Ok(Vec::new());
    };

    Ok(systemd_units_to_suggestions(units, filter))
}

pub struct SystemdUnits;

impl Provider for SystemdUnits {
    fn name(&self) -> &'static str {
        "systemd_units"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "systemctl").await
    }
}

impl SystemdUnits {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        generate_systemd(ctx, binary, SystemdScope::System, SystemdFilter::All).await
    }
}

pub struct SystemdUserUnits;

impl Provider for SystemdUserUnits {
    fn name(&self) -> &'static str {
        "systemd_user_units"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "systemctl").await
    }
}

impl SystemdUserUnits {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        generate_systemd(ctx, binary, SystemdScope::User, SystemdFilter::All).await
    }
}

pub struct SystemdActiveUnits;

impl Provider for SystemdActiveUnits {
    fn name(&self) -> &'static str {
        "systemd_active_units"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "systemctl").await
    }
}

impl SystemdActiveUnits {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        generate_systemd(ctx, binary, SystemdScope::System, SystemdFilter::ActiveOnly).await
    }
}
