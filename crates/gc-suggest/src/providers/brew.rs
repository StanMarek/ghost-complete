// Homebrew native providers for installed formulae, installed casks,
// searchable formulae, searchable casks, and searchable packages
// (the formulae + casks union).

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

/// Decide the `brew search` argv and the parser cap for `query`. Typed
/// queries forward the user's token straight to `brew search <q>` and
/// drop the cap — `brew search` already filters to substring matches,
/// so the cap is only meaningful for the empty-query exploration path
/// that returns the full package list (every formula and cask).
///
/// `flag_prefix` is spliced in between `search` and the query token so a
/// single planner serves the plain (`&[]`), cask-only (`&["--cask"]`),
/// and any future filtered search path with one argv-construction and
/// one cap rule — no per-provider divergence. The empty-query branch
/// still passes a single empty-string argument so `brew search ""` keeps
/// exploring the full list rather than treating the prefix flag as the
/// final positional.
pub(crate) fn brew_search_plan<'a>(
    query: &'a str,
    flag_prefix: &[&'a str],
    empty_cap: usize,
) -> (Vec<&'a str>, usize) {
    let mut args = Vec::with_capacity(2 + flag_prefix.len());
    args.push("search");
    args.extend_from_slice(flag_prefix);
    if query.is_empty() {
        args.push("");
        (args, empty_cap)
    } else {
        args.push(query);
        (args, usize::MAX)
    }
}

/// Detect Homebrew's "nothing matched this query" failure. Modern `brew
/// search <token>` exits non-zero and writes
/// `Error: No formulae or casks found for "<token>".` to stderr when the
/// partial token matches nothing — an expected outcome on the
/// keystroke-to-suggestion hot path, not a command failure. The
/// non-zero exit is surfaced by [`spawn_with_timeout`] as an
/// `anyhow::Error` whose message embeds the trimmed stderr, so we match
/// on the rendered error string. Kept deliberately narrow: genuine
/// failures (timeout, other nonzero exits) do not carry these phrases
/// and stay at `warn`.
fn is_brew_no_match(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("No formula") || message.contains("No formulae") || message.contains("No cask")
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
        Err(error) if is_brew_no_match(&error) => {
            tracing::trace!(binary, error = %error, "brew search found no matches");
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

/// Parse `brew search --cask <query>` output, projecting cask names.
///
/// `brew search --cask <q>` on modern Homebrew (5.x) prints a BARE,
/// header-less token list — it emits no `==> Casks` / `==> Formulae`
/// section headers at all because the `--cask` filter already scopes the
/// result to casks. So this defaults `in_casks` ON (mirroring
/// [`parse_formulae_searchable_output`], which defaults its section flag
/// on): a header-less run treats every line as a cask. A header is only
/// honoured to SUPPRESS — once a non-cask section (`==> Formulae` /
/// legacy `Formulae:`) appears, subsequent lines are dropped until a
/// `==> Casks` / `Casks:` header re-enables emission. This keeps a
/// TTY/legacy `brew search` run that does print headers working while
/// fixing the header-less `--cask` path that previously returned empty.
/// Capped for popup latency on the empty-query exploration path.
pub(crate) fn parse_casks_searchable_output(text: &str, cap: usize) -> Vec<Suggestion> {
    let mut in_casks = true;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("==>") {
            in_casks = line == "==> Casks";
            continue;
        }
        if line.ends_with(':') {
            in_casks = line.eq_ignore_ascii_case("Casks:");
            continue;
        }
        if in_casks {
            for cask in line.split_whitespace() {
                if out.len() >= cap {
                    return out;
                }
                out.push(provider_suggestion(cask, "brew cask"));
            }
        }
    }
    out
}

/// Parse `brew search <query>` output, projecting every token under both
/// the formulae and casks sections. Used when the install/search position
/// is ambiguous and either formula or cask names are acceptable. Capped
/// for popup latency: the empty-query exploration path (`brew search ""`)
/// returns ~16k lines (formulae + casks) on modern Homebrew, so the cap
/// must be honoured here exactly as it is for the formulae-only path.
pub(crate) fn parse_packages_searchable_output(text: &str, cap: usize) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("==>") || line.ends_with(':') {
            continue;
        }
        for token in line.split_whitespace() {
            if out.len() >= cap {
                return out;
            }
            out.push(provider_suggestion(token, "brew formula or cask"));
        }
    }
    out
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
        let (args, cap) = brew_search_plan(ctx.current_token.as_str(), &[], brew_search_cap());
        let Some(output) = run_brew_with_binary(&ctx.cwd, binary, &args).await else {
            return Ok(Vec::new());
        };
        Ok(parse_formulae_searchable_output(&output, cap))
    }
}

pub struct BrewCasksSearchable;

impl Provider for BrewCasksSearchable {
    fn name(&self) -> &'static str {
        "brew_casks_searchable"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "brew").await
    }
}

impl BrewCasksSearchable {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        if !brew_is_supported(binary).await {
            return Ok(Vec::new());
        }
        let (args, cap) =
            brew_search_plan(ctx.current_token.as_str(), &["--cask"], brew_search_cap());
        let Some(output) = run_brew_with_binary(&ctx.cwd, binary, &args).await else {
            return Ok(Vec::new());
        };
        Ok(parse_casks_searchable_output(&output, cap))
    }
}

pub struct BrewPackagesSearchable;

impl Provider for BrewPackagesSearchable {
    fn name(&self) -> &'static str {
        "brew_packages_searchable"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "brew").await
    }
}

impl BrewPackagesSearchable {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        if !brew_is_supported(binary).await {
            return Ok(Vec::new());
        }
        let (args, cap) = brew_search_plan(ctx.current_token.as_str(), &[], brew_search_cap());
        let Some(output) = run_brew_with_binary(&ctx.cwd, binary, &args).await else {
            return Ok(Vec::new());
        };
        Ok(parse_packages_searchable_output(&output, cap))
    }
}
