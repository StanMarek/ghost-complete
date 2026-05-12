// Homebrew native providers for installed formulae, installed casks,
// and searchable formulae.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;

use super::util::{is_binary_missing, spawn_with_timeout};
use super::version_probe;
use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const BREW_TIMEOUT_MS: u64 = 2_000;

pub(crate) const DEFAULT_BREW_SEARCH_CAP: usize = 1_000;

/// Process-global cap on `brew search` output. Set once at engine startup
/// from `[experimental] brew_search_cap` (see [`set_brew_search_cap`]); the
/// `AtomicUsize` avoids threading a fresh `ProviderCtx` field through every
/// provider call site for what is effectively a static knob.
static BREW_SEARCH_CAP: AtomicUsize = AtomicUsize::new(DEFAULT_BREW_SEARCH_CAP);

pub fn set_brew_search_cap(cap: usize) {
    BREW_SEARCH_CAP.store(cap.max(1), Ordering::Relaxed);
}

fn brew_search_cap() -> usize {
    BREW_SEARCH_CAP.load(Ordering::Relaxed)
}

pub(crate) async fn run_brew_with_binary(
    cwd: &Path,
    binary: &str,
    args: &[&str],
) -> Option<String> {
    match spawn_with_timeout(
        cwd,
        binary,
        args.iter().copied(),
        None,
        Duration::from_millis(BREW_TIMEOUT_MS),
    )
    .await
    {
        Ok(stdout) => Some(stdout),
        Err(error) if is_binary_missing(&error) => {
            tracing::trace!(binary, "brew binary not installed");
            None
        }
        Err(error) => {
            tracing::warn!(binary, error = %error, "brew command failed");
            None
        }
    }
}

async fn brew_is_supported(binary: &str) -> bool {
    !matches!(
        version_probe::probe_version(binary, "2.0").await,
        Some(false)
    )
}

pub(crate) fn parse_formulae_installed_output(text: &str) -> Vec<Suggestion> {
    parse_one_per_line(text, "installed brew formula")
}

pub(crate) fn parse_casks_installed_output(text: &str) -> Vec<Suggestion> {
    parse_one_per_line(text, "installed brew cask")
}

pub(crate) fn parse_formulae_searchable_output(text: &str, cap: usize) -> Vec<Suggestion> {
    let mut seen_heading = false;
    let mut in_formulae_section = true;
    let mut suggestions = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with("==>") {
            seen_heading = true;
            in_formulae_section = line == "==> Formulae";
            continue;
        }

        if seen_heading && !in_formulae_section {
            continue;
        }

        for formula in line.split_whitespace() {
            if suggestions.len() >= cap {
                return suggestions;
            }
            suggestions.push(provider_suggestion(formula, "brew formula"));
        }
    }

    suggestions
}

fn parse_one_per_line(text: &str, description: &'static str) -> Vec<Suggestion> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|name| provider_suggestion(name, description))
        .collect()
}

fn provider_suggestion(text: &str, description: &'static str) -> Suggestion {
    Suggestion {
        text: text.to_string(),
        description: Some(description.to_string()),
        kind: SuggestionKind::ProviderValue,
        source: SuggestionSource::Provider,
        ..Default::default()
    }
}

pub struct BrewFormulaeInstalled;

impl Provider for BrewFormulaeInstalled {
    fn name(&self) -> &'static str {
        "brew_formulae_installed"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "brew").await
    }
}

impl BrewFormulaeInstalled {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        if !brew_is_supported(binary).await {
            return Ok(Vec::new());
        }
        let Some(output) = run_brew_with_binary(&ctx.cwd, binary, &["list", "--formula"]).await
        else {
            return Ok(Vec::new());
        };
        Ok(parse_formulae_installed_output(&output))
    }
}

pub struct BrewCasksInstalled;

impl Provider for BrewCasksInstalled {
    fn name(&self) -> &'static str {
        "brew_casks_installed"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "brew").await
    }
}

impl BrewCasksInstalled {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        if !brew_is_supported(binary).await {
            return Ok(Vec::new());
        }
        let Some(output) = run_brew_with_binary(&ctx.cwd, binary, &["list", "--cask"]).await else {
            return Ok(Vec::new());
        };
        Ok(parse_casks_installed_output(&output))
    }
}

pub struct BrewFormulaeSearchable;

impl Provider for BrewFormulaeSearchable {
    fn name(&self) -> &'static str {
        "brew_formulae_searchable"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "brew").await
    }
}

impl BrewFormulaeSearchable {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        if !brew_is_supported(binary).await {
            return Ok(Vec::new());
        }
        let Some(output) = run_brew_with_binary(&ctx.cwd, binary, &["search", ""]).await else {
            return Ok(Vec::new());
        };
        Ok(parse_formulae_searchable_output(&output, brew_search_cap()))
    }
}
