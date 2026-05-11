//! `cargo metadata` providers for local Cargo state.
//!
//! These providers share one `cargo metadata --format-version 1
//! --no-deps` call per cwd and project the stable JSON shape into
//! workspace members, targets, and features. Registry wiring lives in
//! `providers/mod.rs`; this module is intentionally self-contained so
//! it can land alongside parent-owned scaffolding changes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Deserialize;
use tokio::process::Command;

use crate::providers::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const CARGO_METADATA_TIMEOUT: Duration = Duration::from_secs(2);
const CARGO_METADATA_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
struct CargoMetadataCacheEntry {
    inserted_at: Instant,
    stdout: String,
}

static CARGO_METADATA_CACHE: LazyLock<Mutex<HashMap<(PathBuf, String), CargoMetadataCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug, Deserialize)]
struct CargoMetadataOutput {
    #[serde(default)]
    packages: Vec<CargoPackage>,
    #[serde(default)]
    workspace_members: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    #[serde(default)]
    manifest_path: Option<PathBuf>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    targets: Vec<CargoTarget>,
}

#[derive(Clone, Debug, Deserialize)]
struct CargoTarget {
    name: String,
    #[serde(default)]
    kind: Vec<String>,
}

fn parse_cargo_metadata(bytes: &[u8]) -> Option<CargoMetadataOutput> {
    match serde_json::from_slice(bytes) {
        Ok(metadata) => Some(metadata),
        Err(e) => {
            tracing::warn!(error = %e, "cargo metadata JSON parse failed");
            None
        }
    }
}

fn workspace_member_indices(metadata: &CargoMetadataOutput) -> Vec<usize> {
    if metadata.workspace_members.is_empty() {
        return (0..metadata.packages.len()).collect();
    }

    let by_id: HashMap<&str, usize> = metadata
        .packages
        .iter()
        .enumerate()
        .map(|(idx, package)| (package.id.as_str(), idx))
        .collect();

    metadata
        .workspace_members
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .collect()
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

/// Parse package names for `cargo_workspace_members` from raw
/// `cargo metadata` JSON. Malformed JSON or unknown package ids degrade
/// to an empty suggestion set.
pub(crate) fn parse_cargo_workspace_members(bytes: &[u8]) -> Vec<Suggestion> {
    let Some(metadata) = parse_cargo_metadata(bytes) else {
        return Vec::new();
    };

    workspace_member_indices(&metadata)
        .into_iter()
        .filter_map(|idx| metadata.packages.get(idx))
        .filter(|package| !package.name.is_empty())
        .map(|package| provider_suggestion(package.name.clone(), None))
        .collect()
}

/// Parse target names for `cargo_targets`, filtered by Cargo target
/// kind (`bin`, `example`, `test`, `bench`, `lib`).
pub(crate) fn parse_cargo_targets(bytes: &[u8], kind: &str) -> Vec<Suggestion> {
    let Some(metadata) = parse_cargo_metadata(bytes) else {
        return Vec::new();
    };
    if kind.is_empty() {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut suggestions = Vec::new();
    for idx in workspace_member_indices(&metadata) {
        let Some(package) = metadata.packages.get(idx) else {
            continue;
        };
        for target in &package.targets {
            if target.name.is_empty() || !target.kind.iter().any(|k| k == kind) {
                continue;
            }
            if seen.insert(target.name.clone()) {
                suggestions.push(provider_suggestion(
                    target.name.clone(),
                    Some(package.name.clone()),
                ));
            }
        }
    }
    suggestions
}

/// Parse feature names for `cargo_features` from the package whose
/// manifest is the nearest ancestor of `cwd`. Falls back to the first
/// workspace member when `cwd` does not sit under a package manifest
/// directory, which keeps workspace-root completions useful.
pub(crate) fn parse_cargo_features(bytes: &[u8], cwd: &Path) -> Vec<Suggestion> {
    let Some(metadata) = parse_cargo_metadata(bytes) else {
        return Vec::new();
    };

    let Some(package) = active_package_for_cwd(&metadata, cwd) else {
        return Vec::new();
    };

    package
        .features
        .keys()
        .filter(|name| !name.is_empty())
        .cloned()
        .map(|name| provider_suggestion(name, None))
        .collect()
}

fn active_package_for_cwd<'a>(
    metadata: &'a CargoMetadataOutput,
    cwd: &Path,
) -> Option<&'a CargoPackage> {
    let mut best: Option<(usize, &CargoPackage)> = None;
    for idx in workspace_member_indices(metadata) {
        let Some(package) = metadata.packages.get(idx) else {
            continue;
        };
        let Some(manifest_path) = &package.manifest_path else {
            continue;
        };
        let Some(package_dir) = manifest_path.parent() else {
            continue;
        };
        if cwd.starts_with(package_dir) {
            let depth = package_dir.components().count();
            if best.is_none_or(|(best_depth, _)| depth > best_depth) {
                best = Some((depth, package));
            }
        }
    }

    best.map(|(_, package)| package).or_else(|| {
        workspace_member_indices(metadata)
            .into_iter()
            .find_map(|idx| metadata.packages.get(idx))
    })
}

async fn metadata_stdout_with_binary(cwd: &Path, binary: &str) -> Option<String> {
    let key = (cwd.to_path_buf(), binary.to_string());
    {
        let guard = match CARGO_METADATA_CACHE.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::error!("cargo metadata cache mutex poisoned; recovering");
                poisoned.into_inner()
            }
        };
        if let Some(entry) = guard.get(&key) {
            if entry.inserted_at.elapsed() < CARGO_METADATA_TTL {
                return Some(entry.stdout.clone());
            }
        }
    }

    let stdout = run_cargo_metadata_with_binary(cwd, binary).await?;
    let mut guard = match CARGO_METADATA_CACHE.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::error!("cargo metadata cache mutex poisoned; recovering");
            poisoned.into_inner()
        }
    };
    guard.insert(
        key,
        CargoMetadataCacheEntry {
            inserted_at: Instant::now(),
            stdout: stdout.clone(),
        },
    );
    Some(stdout)
}

async fn run_cargo_metadata_with_binary(cwd: &Path, binary: &str) -> Option<String> {
    let output = match tokio::time::timeout(
        CARGO_METADATA_TIMEOUT,
        Command::new(binary)
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            tracing::warn!(binary, error = %e, "cargo metadata command failed to start");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                binary,
                timeout_ms = CARGO_METADATA_TIMEOUT.as_millis(),
                "cargo metadata command timed out"
            );
            return None;
        }
    };

    if !output.status.success() {
        tracing::warn!(
            binary,
            exit = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "cargo metadata command failed"
        );
        return None;
    }

    match String::from_utf8(output.stdout) {
        Ok(stdout) => Some(stdout),
        Err(e) => {
            tracing::warn!(binary, error = %e, "cargo metadata stdout was not UTF-8");
            None
        }
    }
}

pub struct CargoTargets;

impl Provider for CargoTargets {
    fn name(&self) -> &'static str {
        "cargo_targets"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "cargo").await
    }
}

impl CargoTargets {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(stdout) = metadata_stdout_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        let kind = ctx.params.get("kind").map(String::as_str).unwrap_or("bin");
        Ok(parse_cargo_targets(stdout.as_bytes(), kind))
    }
}

pub struct CargoFeatures;

impl Provider for CargoFeatures {
    fn name(&self) -> &'static str {
        "cargo_features"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "cargo").await
    }
}

impl CargoFeatures {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(stdout) = metadata_stdout_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        Ok(parse_cargo_features(stdout.as_bytes(), &ctx.cwd))
    }
}

pub struct CargoWorkspaceMembers;

impl Provider for CargoWorkspaceMembers {
    fn name(&self) -> &'static str {
        "cargo_workspace_members"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "cargo").await
    }
}

impl CargoWorkspaceMembers {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(stdout) = metadata_stdout_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        Ok(parse_cargo_workspace_members(stdout.as_bytes()))
    }
}
