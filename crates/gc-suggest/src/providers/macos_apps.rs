//! macOS `open -a` and `open -b` native providers, sourced from
//! Spotlight metadata via `mdfind` + `mdls`. Graceful empty on
//! Spotlight-disabled hosts and on non-macOS targets where the
//! binaries don't exist (spawn-time `ENOENT` → `Ok(vec![])`).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Semaphore;

use super::util::{is_binary_missing, spawn_with_timeout};
use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const MDFIND_TIMEOUT_MS: u64 = 2_000;
const MDLS_TIMEOUT_MS: u64 = 1_000;
const MDFIND_QUERY: &str = "kMDItemContentType == 'com.apple.application-bundle'";

/// Maximum number of `.app` paths to enumerate when resolving bundle
/// identifiers. `mdls` spawns one subprocess per path, so we cap to
/// keep the worst-case wall-clock bounded — 500 covers a stock macOS
/// install comfortably and stays well under the popup-render budget.
const BUNDLE_ID_RESOLVE_CAP: usize = 500;

/// Maximum number of concurrent `mdls` subprocesses when resolving
/// bundle identifiers. Bounds the spawn fan-out so we don't launch
/// [`BUNDLE_ID_RESOLVE_CAP`] processes at once while still collapsing
/// the serial wall-clock cost.
const MDLS_CONCURRENCY: usize = 16;

/// Extract display names from `mdfind` output. Each line is an
/// absolute `.app` path; the file stem is the app's display name
/// (e.g. `/Applications/Safari.app` → `Safari`). Returns pairs of
/// `(display_name, full_path)` so callers can surface the path as
/// the suggestion description.
pub fn parse_applications(mdfind: &str) -> Vec<(String, String)> {
    mdfind
        .lines()
        .map(str::trim)
        .filter(|l| l.ends_with(".app"))
        .filter_map(|p| {
            let stem = Path::new(p).file_stem()?.to_str()?.to_string();
            Some((stem, p.to_string()))
        })
        .collect()
}

/// Parse one or more `mdls -name kMDItemCFBundleIdentifier` lines.
/// Filters out the literal `(null)` (Spotlight's sentinel for a
/// missing key) and deduplicates by bundle id so re-runs of `mdfind`
/// that return the same bundle from `/Applications` and a Cache
/// directory don't surface twice.
pub fn parse_bundle_identifiers(mdls: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in mdls.lines() {
        let candidate = match line.split_once('=') {
            Some((_, rhs)) => rhs.trim().trim_matches('"').to_string(),
            None => line.trim().trim_matches('"').to_string(),
        };
        if candidate.is_empty() || candidate == "(null)" {
            continue;
        }
        if seen.insert(candidate.clone()) {
            out.push(candidate);
        }
    }
    out
}

/// Enumerate the `.app` paths to resolve from raw `mdfind` output:
/// trim each line, keep only `.app` entries, and cap at `cap`. Pure
/// (no subprocess) so the cap and `.app` filter can be unit-tested.
pub fn app_paths_to_resolve(mdfind_stdout: &str, cap: usize) -> Vec<&str> {
    mdfind_stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.ends_with(".app"))
        .take(cap)
        .collect()
}

/// Assemble bundle-identifier suggestions from per-path `mdls` outputs,
/// presented in the original `paths` order. `raw_outputs[i]` holds the
/// `mdls -raw` stdout for `paths[i]` (or `None` if that lookup failed).
/// Applies the same parse + cross-path dedup as the live path: first
/// path to yield a given bundle id wins and supplies the description.
fn assemble_bundle_identifiers(paths: &[&str], raw_outputs: &[Option<String>]) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (path, raw) in paths.iter().zip(raw_outputs.iter()) {
        let Some(raw) = raw else {
            continue;
        };
        for bid in parse_bundle_identifiers(raw) {
            if seen.insert(bid.clone()) {
                out.push(Suggestion {
                    text: bid,
                    description: Some(path.to_string()),
                    kind: SuggestionKind::ProviderValue,
                    source: SuggestionSource::Provider,
                    ..Default::default()
                });
            }
        }
    }
    out
}

async fn run_mdfind(cwd: &Path, binary: &str) -> Option<String> {
    match spawn_with_timeout(
        cwd,
        binary,
        [MDFIND_QUERY],
        None,
        Duration::from_millis(MDFIND_TIMEOUT_MS),
    )
    .await
    {
        Ok(stdout) => Some(stdout),
        Err(e) if is_binary_missing(&e) => {
            tracing::trace!(binary, "mdfind binary not installed");
            None
        }
        Err(e) => {
            tracing::warn!(binary, error = %e, "mdfind for applications failed");
            None
        }
    }
}

async fn run_mdls_bundle_id(cwd: &Path, binary: &str, app_path: &str) -> Option<String> {
    match spawn_with_timeout(
        cwd,
        binary,
        ["-name", "kMDItemCFBundleIdentifier", "-raw", app_path],
        None,
        Duration::from_millis(MDLS_TIMEOUT_MS),
    )
    .await
    {
        Ok(stdout) => Some(stdout),
        Err(e) if is_binary_missing(&e) => {
            tracing::trace!(app_path, "mdls binary not installed");
            None
        }
        Err(e) => {
            tracing::warn!(app_path, error = %e, "mdls bundle id lookup failed");
            None
        }
    }
}

/// `macos_applications` — enumerates installed `.app` bundles via
/// Spotlight. Suggestion text is the display name (`Safari`) and the
/// description is the full bundle path — matches how `open -a` accepts
/// either form.
pub struct MacosApplications;

impl Provider for MacosApplications {
    fn name(&self) -> &'static str {
        "macos_applications"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binaries(ctx, "mdfind", "mdls").await
    }
}

impl MacosApplications {
    pub(crate) async fn generate_with_binaries(
        &self,
        ctx: &ProviderCtx,
        mdfind_binary: &str,
        _mdls_binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(stdout) = run_mdfind(&ctx.cwd, mdfind_binary).await else {
            return Ok(Vec::new());
        };
        let apps = parse_applications(&stdout);
        Ok(apps
            .into_iter()
            .map(|(name, path)| Suggestion {
                text: name,
                description: Some(path),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
            .collect())
    }
}

/// `macos_bundle_identifiers` — enumerates installed application
/// bundle identifiers via Spotlight (`mdfind` for paths, `mdls -raw`
/// per path for the bundle id). Capped at [`BUNDLE_ID_RESOLVE_CAP`]
/// to keep the worst-case latency bounded.
pub struct MacosBundleIdentifiers;

impl Provider for MacosBundleIdentifiers {
    fn name(&self) -> &'static str {
        "macos_bundle_identifiers"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binaries(ctx, "mdfind", "mdls").await
    }
}

impl MacosBundleIdentifiers {
    pub(crate) async fn generate_with_binaries(
        &self,
        ctx: &ProviderCtx,
        mdfind_binary: &str,
        mdls_binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(stdout) = run_mdfind(&ctx.cwd, mdfind_binary).await else {
            return Ok(Vec::new());
        };
        let paths = app_paths_to_resolve(&stdout, BUNDLE_ID_RESOLVE_CAP);
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        // Resolve the per-path `mdls` lookups with bounded concurrency
        // instead of serially (up to BUNDLE_ID_RESOLVE_CAP back-to-back
        // subprocess spawns would cost multiple seconds wall-clock). A
        // semaphore caps live `mdls` processes at MDLS_CONCURRENCY; each
        // task carries its input index so we can reassemble in the
        // original order and preserve the cross-path first-wins dedup.
        let semaphore = Arc::new(Semaphore::new(MDLS_CONCURRENCY));
        let mut join_set = tokio::task::JoinSet::new();
        for (idx, path) in paths.iter().enumerate() {
            let cwd = ctx.cwd.clone();
            let binary = mdls_binary.to_string();
            let path = path.to_string();
            let semaphore = Arc::clone(&semaphore);
            join_set.spawn(async move {
                // `Semaphore::acquire_owned` only errors if the
                // semaphore is closed; we never close it, so this is
                // infallible here.
                let _permit = semaphore.acquire_owned().await;
                (idx, run_mdls_bundle_id(&cwd, &binary, &path).await)
            });
        }

        let mut raw_outputs: Vec<Option<String>> = vec![None; paths.len()];
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((idx, raw)) => raw_outputs[idx] = raw,
                Err(e) => tracing::warn!(error = %e, "mdls resolve task panicked"),
            }
        }

        let attempted = paths.len();
        let resolved = raw_outputs.iter().filter(|r| r.is_some()).count();
        let out = assemble_bundle_identifiers(&paths, &raw_outputs);
        if out.is_empty() && attempted > 0 && resolved < attempted {
            tracing::warn!(
                attempted,
                "mdls resolved zero bundle identifiers from application paths"
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    fn ctx(cwd: &Path) -> ProviderCtx {
        ProviderCtx {
            cwd: cwd.to_path_buf(),
            env: Arc::new(HashMap::new()),
            current_token: String::new(),
            params: Arc::new(BTreeMap::new()),
        }
    }

    #[test]
    fn parse_applications_extracts_display_names_from_paths() {
        let mdfind = "/Applications/Safari.app\n/Applications/Terminal.app\n/Applications/Visual Studio Code.app\n";
        let parsed = parse_applications(mdfind);
        let names: Vec<&str> = parsed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Safari", "Terminal", "Visual Studio Code"]);
        let paths: Vec<&str> = parsed.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/Applications/Safari.app",
                "/Applications/Terminal.app",
                "/Applications/Visual Studio Code.app",
            ]
        );
    }

    #[test]
    fn parse_applications_ignores_non_app_lines() {
        let mdfind = "/Applications/Safari.app\n\nrandom-text\n/tmp/not-an-app\n";
        let parsed = parse_applications(mdfind);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "Safari");
    }

    #[test]
    fn parse_bundle_identifiers_strips_quotes_filters_null_dedupes() {
        let mdls = "kMDItemCFBundleIdentifier = \"com.apple.Safari\"\nkMDItemCFBundleIdentifier = \"(null)\"\nkMDItemCFBundleIdentifier = \"com.apple.Safari\"\nkMDItemCFBundleIdentifier = \"com.apple.Terminal\"\n";
        let parsed = parse_bundle_identifiers(mdls);
        assert_eq!(parsed, vec!["com.apple.Safari", "com.apple.Terminal"]);
    }

    #[test]
    fn parse_bundle_identifiers_accepts_raw_mdls_output() {
        // `mdls -raw` emits the bare value with no `key =` prefix.
        let raw = "com.apple.Safari\n";
        let parsed = parse_bundle_identifiers(raw);
        assert_eq!(parsed, vec!["com.apple.Safari"]);
    }

    #[test]
    fn app_paths_to_resolve_filters_non_app_lines_and_trims() {
        let mdfind = "  /Applications/Safari.app  \nrandom-noise\n\n/tmp/not-an-app\n/Applications/Terminal.app\n";
        let paths = app_paths_to_resolve(mdfind, BUNDLE_ID_RESOLVE_CAP);
        assert_eq!(
            paths,
            vec!["/Applications/Safari.app", "/Applications/Terminal.app"]
        );
    }

    #[test]
    fn app_paths_to_resolve_applies_cap_keeping_only_app_lines() {
        // 600 `.app` lines interleaved with non-`.app` noise.
        let mut mdfind = String::new();
        for i in 0..600 {
            mdfind.push_str(&format!("/Applications/App{i}.app\n"));
            mdfind.push_str("noise-line\n");
        }
        let cap = 500;
        let paths = app_paths_to_resolve(&mdfind, cap);
        assert_eq!(paths.len(), cap);
        assert!(paths.iter().all(|p| p.ends_with(".app")));
        assert_eq!(paths[0], "/Applications/App0.app");
        assert_eq!(paths[cap - 1], "/Applications/App499.app");
    }

    #[test]
    fn assemble_bundle_identifiers_dedupes_across_paths_first_wins() {
        let paths = vec!["/Applications/Safari.app", "/Applications/Safari copy.app"];
        let raw_outputs = vec![
            Some("com.apple.Safari\n".to_string()),
            Some("com.apple.Safari\n".to_string()),
        ];
        let suggestions = assemble_bundle_identifiers(&paths, &raw_outputs);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "com.apple.Safari");
        // First path that yielded the bundle id supplies the description.
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("/Applications/Safari.app")
        );
    }

    #[test]
    fn assemble_bundle_identifiers_skips_failed_lookups_and_preserves_order() {
        let paths = vec![
            "/Applications/Safari.app",
            "/Applications/Missing.app",
            "/Applications/Terminal.app",
        ];
        let raw_outputs = vec![
            Some("com.apple.Safari\n".to_string()),
            None, // mdls failed for this path
            Some("com.apple.Terminal\n".to_string()),
        ];
        let suggestions = assemble_bundle_identifiers(&paths, &raw_outputs);
        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["com.apple.Safari", "com.apple.Terminal"]);
    }

    #[tokio::test]
    async fn macos_applications_returns_ok_empty_when_mdfind_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let suggestions = MacosApplications
            .generate_with_binaries(
                &ctx(tmp.path()),
                "/nonexistent/mdfind-for-test",
                "/nonexistent/mdls-for-test",
            )
            .await
            .unwrap();
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn macos_bundle_identifiers_returns_ok_empty_when_mdfind_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let suggestions = MacosBundleIdentifiers
            .generate_with_binaries(
                &ctx(tmp.path()),
                "/nonexistent/mdfind-for-test",
                "/nonexistent/mdls-for-test",
            )
            .await
            .unwrap();
        assert!(suggestions.is_empty());
    }
}
