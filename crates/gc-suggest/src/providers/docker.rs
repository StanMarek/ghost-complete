// Docker-family native providers.
//
// These providers replace Fig generators that shell out to Docker-like
// CLIs and parse `--format '{{json .}}'` line-delimited JSON. The
// command binary is read from `ctx.params["binary"]` and defaults to
// `docker`, which lets specs reuse the same parser for `podman`-style
// output.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use super::util::{parse_json_lines, spawn_with_timeout};
use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const DOCKER_TIMEOUT_MS: u64 = 2_000;
const JSON_FORMAT: &str = "{{json .}}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockerKind {
    Images,
    Containers,
    RunningContainers,
    Networks,
    Volumes,
}

impl DockerKind {
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Images => &["images", "--format", JSON_FORMAT],
            Self::Containers => &["ps", "-a", "--format", JSON_FORMAT],
            Self::RunningContainers => {
                &["ps", "--filter", "status=running", "--format", JSON_FORMAT]
            }
            Self::Networks => &["network", "ls", "--format", JSON_FORMAT],
            Self::Volumes => &["volume", "ls", "--format", JSON_FORMAT],
        }
    }
}

#[derive(Debug, Deserialize)]
struct DockerRow {
    #[serde(default, alias = "Repository")]
    repository: Option<String>,
    #[serde(default, alias = "Tag")]
    tag: Option<String>,
    #[serde(default, alias = "ID", alias = "Id")]
    id: Option<String>,
    #[serde(default, alias = "Image")]
    image: Option<String>,
    #[serde(default, alias = "Names")]
    names: Option<String>,
    #[serde(default, alias = "Name")]
    name: Option<String>,
    #[serde(default, alias = "Status")]
    status: Option<String>,
    #[serde(default, alias = "State")]
    state: Option<String>,
    #[serde(default, alias = "Size")]
    size: Option<String>,
    #[serde(default, alias = "Driver")]
    driver: Option<String>,
    #[serde(default, alias = "Scope")]
    scope: Option<String>,
    #[serde(default, alias = "Mountpoint")]
    mountpoint: Option<String>,
}

pub(crate) fn parse_docker_json_lines(stdout: &str, kind: DockerKind) -> Vec<Suggestion> {
    let rows = match parse_rows(stdout) {
        Some(rows) => rows,
        None => return Vec::new(),
    };

    rows.into_iter()
        .filter_map(|row| row_to_suggestion(row, kind))
        .collect()
}

fn parse_rows(stdout: &str) -> Option<Vec<DockerRow>> {
    parse_json_lines(stdout).ok()
}

fn row_to_suggestion(row: DockerRow, kind: DockerKind) -> Option<Suggestion> {
    if kind == DockerKind::RunningContainers && !is_running(&row) {
        return None;
    }

    let (text, description) = match kind {
        DockerKind::Images => image_text_and_description(&row)?,
        DockerKind::Containers | DockerKind::RunningContainers => {
            container_text_and_description(&row)?
        }
        DockerKind::Networks => named_text_and_description(
            first_clean([row.name.as_deref(), row.names.as_deref()])?,
            [
                row.id.as_deref(),
                row.driver.as_deref(),
                row.scope.as_deref(),
            ],
        ),
        DockerKind::Volumes => named_text_and_description(
            first_clean([row.name.as_deref()])?,
            [
                row.driver.as_deref(),
                row.mountpoint.as_deref(),
                row.scope.as_deref(),
            ],
        ),
    };

    Some(Suggestion {
        text,
        description,
        kind: SuggestionKind::ProviderValue,
        source: SuggestionSource::Provider,
        ..Default::default()
    })
}

fn image_text_and_description(row: &DockerRow) -> Option<(String, Option<String>)> {
    let repository = clean(row.repository.as_deref());
    let tag = clean(row.tag.as_deref());
    let id = clean(row.id.as_deref());

    let text = match (repository, tag, id) {
        (Some(repository), Some(tag), _) => format!("{repository}:{tag}"),
        (Some(repository), None, _) => repository.to_string(),
        (None, _, Some(id)) => id.to_string(),
        (None, _, None) => return None,
    };
    let description = join_description([id, clean(row.size.as_deref())]);

    Some((text, description))
}

fn container_text_and_description(row: &DockerRow) -> Option<(String, Option<String>)> {
    let text = first_clean([row.names.as_deref(), row.name.as_deref(), row.id.as_deref()])?;
    let image = clean(row.image.as_deref());
    let status = clean(row.status.as_deref()).or_else(|| clean(row.state.as_deref()));
    let description = match (image, status) {
        (Some(image), Some(status)) => Some(format!("{image} ({status})")),
        (Some(image), None) => Some(image.to_string()),
        (None, Some(status)) => Some(status.to_string()),
        (None, None) => None,
    };

    Some((text.to_string(), description))
}

fn named_text_and_description<'a, I>(
    text: &'a str,
    description_parts: I,
) -> (String, Option<String>)
where
    I: IntoIterator<Item = Option<&'a str>>,
{
    (text.to_string(), join_description(description_parts))
}

fn is_running(row: &DockerRow) -> bool {
    clean(row.state.as_deref()).is_some_and(|state| state.eq_ignore_ascii_case("running"))
        || clean(row.status.as_deref()).is_some_and(|status| {
            status.eq_ignore_ascii_case("running") || status.starts_with("Up ")
        })
}

fn first_clean<'a, I>(values: I) -> Option<&'a str>
where
    I: IntoIterator<Item = Option<&'a str>>,
{
    values.into_iter().find_map(clean)
}

fn join_description<'a, I>(parts: I) -> Option<String>
where
    I: IntoIterator<Item = Option<&'a str>>,
{
    let parts: Vec<&str> = parts.into_iter().filter_map(clean).collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn clean(value: Option<&str>) -> Option<&str> {
    let value = value?.trim();
    if value.is_empty() || value == "<none>" || value == "N/A" {
        None
    } else {
        Some(value)
    }
}

async fn run_docker_json_lines_with_binary(
    cwd: &Path,
    binary: &str,
    kind: DockerKind,
) -> Option<String> {
    match spawn_with_timeout(
        cwd,
        binary,
        kind.args().iter().copied(),
        None,
        Duration::from_millis(DOCKER_TIMEOUT_MS),
    )
    .await
    {
        Ok(stdout) => Some(stdout),
        Err(error) => {
            tracing::warn!(binary, error = %error, "docker provider command failed");
            None
        }
    }
}

async fn generate_docker(ctx: &ProviderCtx, kind: DockerKind) -> Result<Vec<Suggestion>> {
    let binary = ctx
        .params
        .get("binary")
        .map(String::as_str)
        .map(str::trim)
        .filter(|binary| !binary.is_empty())
        .unwrap_or("docker");

    let Some(stdout) = run_docker_json_lines_with_binary(&ctx.cwd, binary, kind).await else {
        return Ok(Vec::new());
    };

    Ok(parse_docker_json_lines(&stdout, kind))
}

pub struct DockerImages;

impl Provider for DockerImages {
    fn name(&self) -> &'static str {
        "docker_images"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        generate_docker(ctx, DockerKind::Images).await
    }
}

pub struct DockerContainers;

impl Provider for DockerContainers {
    fn name(&self) -> &'static str {
        "docker_containers"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        generate_docker(ctx, DockerKind::Containers).await
    }
}

pub struct DockerRunningContainers;

impl Provider for DockerRunningContainers {
    fn name(&self) -> &'static str {
        "docker_running_containers"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        generate_docker(ctx, DockerKind::RunningContainers).await
    }
}

pub struct DockerNetworks;

impl Provider for DockerNetworks {
    fn name(&self) -> &'static str {
        "docker_networks"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        generate_docker(ctx, DockerKind::Networks).await
    }
}

pub struct DockerVolumes;

impl Provider for DockerVolumes {
    fn name(&self) -> &'static str {
        "docker_volumes"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        generate_docker(ctx, DockerKind::Volumes).await
    }
}
