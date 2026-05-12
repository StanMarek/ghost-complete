use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;

use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

use super::{Provider, ProviderCtx};

const PROFILE_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProfileCacheKey {
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    credentials_path: PathBuf,
    credentials_mtime: Option<SystemTime>,
}

#[derive(Clone)]
struct ProfileCacheEntry {
    inserted_at: Instant,
    suggestions: Vec<Suggestion>,
}

static PROFILE_CACHE: LazyLock<Mutex<HashMap<ProfileCacheKey, ProfileCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct AwsProfileNames;

impl Provider for AwsProfileNames {
    fn name(&self) -> &'static str {
        "aws_profile_names"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        Ok(profile_suggestions(ctx))
    }
}

fn profile_suggestions(ctx: &ProviderCtx) -> Vec<Suggestion> {
    let config_path = aws_config_path(ctx);
    let credentials_path = aws_credentials_path(ctx);
    let key = ProfileCacheKey {
        config_mtime: file_mtime(&config_path),
        credentials_mtime: file_mtime(&credentials_path),
        config_path,
        credentials_path,
    };

    {
        let guard = match PROFILE_CACHE.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::error!("aws profile cache mutex poisoned; recovering");
                poisoned.into_inner()
            }
        };
        if let Some(entry) = guard.get(&key) {
            if entry.inserted_at.elapsed() < PROFILE_CACHE_TTL {
                return entry.suggestions.clone();
            }
        }
    }

    let suggestions = read_profile_names(&key.config_path, &key.credentials_path)
        .into_iter()
        .map(|name| provider_suggestion(name, Some("aws profile".to_string())))
        .collect::<Vec<_>>();

    let mut guard = match PROFILE_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("aws profile cache mutex poisoned; recovering");
            poisoned.into_inner()
        }
    };
    guard.insert(
        key,
        ProfileCacheEntry {
            inserted_at: Instant::now(),
            suggestions: suggestions.clone(),
        },
    );
    suggestions
}

fn read_profile_names(config_path: &Path, credentials_path: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Ok(contents) = std::fs::read_to_string(config_path) {
        parse_config_profiles(&contents, &mut names);
    }
    if let Ok(contents) = std::fs::read_to_string(credentials_path) {
        parse_credentials_profiles(&contents, &mut names);
    }
    names
}

fn parse_config_profiles(contents: &str, names: &mut BTreeSet<String>) {
    for section in ini_sections(contents) {
        if section == "default" {
            names.insert(section.to_string());
        } else if let Some(profile) = section.strip_prefix("profile ").and_then(clean) {
            names.insert(profile.to_string());
        }
    }
}

fn parse_credentials_profiles(contents: &str, names: &mut BTreeSet<String>) {
    for section in ini_sections(contents) {
        if let Some(profile) = clean(section) {
            names.insert(profile.to_string());
        }
    }
}

fn ini_sections(contents: &str) -> impl Iterator<Item = &str> {
    contents.lines().filter_map(|line| {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            return None;
        }
        line.strip_prefix('[')
            .and_then(|line| line.split_once(']').map(|(section, _)| section.trim()))
            .and_then(clean)
    })
}

fn aws_config_path(ctx: &ProviderCtx) -> PathBuf {
    ctx.env("AWS_CONFIG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_aws_path("config"))
}

fn aws_credentials_path(ctx: &ProviderCtx) -> PathBuf {
    ctx.env("AWS_SHARED_CREDENTIALS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_aws_path("credentials"))
}

fn home_aws_path(file_name: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aws")
        .join(file_name)
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
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
