//! Local `package.json` dependency providers.
//!
//! `npm_dependencies`, `npm_dev_dependencies`, and
//! `npm_all_dependencies` read the nearest ancestor `package.json`
//! directly instead of spawning Node or npm. The lookup shape mirrors
//! the existing `npm_scripts` provider: walk ancestors from `ctx.cwd`,
//! parse one JSON object, and degrade to an empty suggestion set on
//! missing or malformed input.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::providers::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const MAX_ANCESTOR_WALK: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum NpmDependencyMode {
    Dependencies,
    DevDependencies,
    AllDependencies,
}

impl NpmDependencyMode {
    fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Dependencies => &["dependencies"],
            Self::DevDependencies => &["devDependencies"],
            Self::AllDependencies => &["dependencies", "devDependencies"],
        }
    }
}

#[derive(Clone)]
struct NpmDependencyCacheEntry {
    mtime: Option<SystemTime>,
    size: u64,
    suggestions: Vec<Suggestion>,
}

static NPM_DEPENDENCY_CACHE: LazyLock<
    Mutex<HashMap<(PathBuf, NpmDependencyMode), NpmDependencyCacheEntry>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn provider_suggestion(text: String, description: Option<String>) -> Suggestion {
    Suggestion {
        text,
        description,
        kind: SuggestionKind::ProviderValue,
        source: SuggestionSource::Provider,
        ..Default::default()
    }
}

/// Parse dependency names from `package.json` bytes. Values are carried
/// as descriptions when they are strings; non-string dependency entries
/// are skipped because npm dependency specifiers are string-valued.
pub(crate) fn parse_npm_dependencies(bytes: &[u8], mode: NpmDependencyMode) -> Vec<Suggestion> {
    let root: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(error = %e, "package.json parse failed");
            return Vec::new();
        }
    };
    let Some(root) = root.as_object() else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut suggestions = Vec::new();
    for field in mode.fields() {
        append_dependency_field(root, field, &mut seen, &mut suggestions);
    }
    suggestions
}

fn append_dependency_field(
    root: &Map<String, Value>,
    field: &str,
    seen: &mut HashSet<String>,
    suggestions: &mut Vec<Suggestion>,
) {
    let Some(value) = root.get(field) else {
        return;
    };
    let Some(dependencies) = value.as_object() else {
        tracing::warn!(field, "package.json dependency field is not an object");
        return;
    };

    for (name, version) in dependencies {
        if name.is_empty() || seen.contains(name) {
            continue;
        }
        let Some(version) = version.as_str() else {
            tracing::warn!(field, dependency = %name, "package.json dependency value is not a string");
            continue;
        };
        seen.insert(name.clone());
        suggestions.push(provider_suggestion(name.clone(), Some(version.to_string())));
    }
}

fn find_package_json(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    for _ in 0..MAX_ANCESTOR_WALK {
        let dir = current?;
        let candidate = dir.join("package.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

fn cached_dependencies(path: &Path, mode: NpmDependencyMode) -> Option<Vec<Suggestion>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "package.json metadata read failed");
            return None;
        }
    };
    let mtime = match metadata.modified() {
        Ok(mtime) => Some(mtime),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "package.json mtime unavailable; re-reading without cache"
            );
            None
        }
    };
    let size = metadata.len();
    let key = (path.to_path_buf(), mode);

    if let Some(probe_mtime) = mtime {
        let guard = match NPM_DEPENDENCY_CACHE.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::error!("npm dependency cache mutex poisoned; recovering");
                poisoned.into_inner()
            }
        };
        if let Some(entry) = guard.get(&key) {
            if entry.mtime == Some(probe_mtime) && entry.size == size {
                return Some(entry.suggestions.clone());
            }
        }
    }

    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "package.json read failed");
            return None;
        }
    };
    let suggestions = parse_npm_dependencies(&bytes, mode);

    if mtime.is_none() {
        return Some(suggestions);
    }

    let mut guard = match NPM_DEPENDENCY_CACHE.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::error!("npm dependency cache mutex poisoned; recovering");
            poisoned.into_inner()
        }
    };
    guard.insert(
        key,
        NpmDependencyCacheEntry {
            mtime,
            size,
            suggestions: suggestions.clone(),
        },
    );
    Some(suggestions)
}

async fn generate_with_root(root: &Path, mode: NpmDependencyMode) -> Result<Vec<Suggestion>> {
    let Some(path) = find_package_json(root) else {
        return Ok(Vec::new());
    };
    Ok(cached_dependencies(&path, mode).unwrap_or_default())
}

pub struct NpmDependencies;

impl Provider for NpmDependencies {
    fn name(&self) -> &'static str {
        "npm_dependencies"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_root(&ctx.cwd).await
    }
}

impl NpmDependencies {
    pub(crate) async fn generate_with_root(&self, root: &Path) -> Result<Vec<Suggestion>> {
        generate_with_root(root, NpmDependencyMode::Dependencies).await
    }
}

pub struct NpmDevDependencies;

impl Provider for NpmDevDependencies {
    fn name(&self) -> &'static str {
        "npm_dev_dependencies"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_root(&ctx.cwd).await
    }
}

impl NpmDevDependencies {
    pub(crate) async fn generate_with_root(&self, root: &Path) -> Result<Vec<Suggestion>> {
        generate_with_root(root, NpmDependencyMode::DevDependencies).await
    }
}

pub struct NpmAllDependencies;

impl Provider for NpmAllDependencies {
    fn name(&self) -> &'static str {
        "npm_all_dependencies"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_root(&ctx.cwd).await
    }
}

impl NpmAllDependencies {
    pub(crate) async fn generate_with_root(&self, root: &Path) -> Result<Vec<Suggestion>> {
        generate_with_root(root, NpmDependencyMode::AllDependencies).await
    }
}
