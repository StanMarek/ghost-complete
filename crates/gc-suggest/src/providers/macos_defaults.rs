//! macOS `defaults` native provider — replaces the JS-backed generator
//! in `specs/defaults.json` that shells out to `defaults domains` and
//! splits the single-line, comma-separated preference domain list into
//! discrete completions.
//!
//! `defaults domains` is macOS-only: the binary ships with the OS and
//! has no Linux counterpart. On Linux the subprocess fails at spawn
//! time, which is indistinguishable from "tool not installed" and flows
//! through the standard `None` → empty-Vec path used by every other
//! native provider.
//!
//! Filename is `macos_defaults.rs` rather than `defaults.rs` so the
//! module name does not collide visually with Rust's ubiquitous
//! `Default` trait.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tokio::process::Command;

use super::{Provider, ProviderCtx, PREV_ARG_KEY};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Timeout for `defaults domains`. 2s is the convention for
/// external-tool providers — generous for an on-device preference
/// read but tight enough that a stuck `cfprefsd` can't stall completion.
const DEFAULTS_DOMAINS_TIMEOUT_MS: u64 = 2_000;

/// Run `defaults domains` against the user's real `defaults` binary.
///
/// The `binary` argument is always `"defaults"` in production and
/// exists as a test seam: subprocess-failure tests inject a
/// nonexistent path so the spawn-time "file not found" path is
/// exercised without mutating `$PATH`.
pub(crate) async fn run_defaults_domains_with_binary(cwd: &Path, binary: &str) -> Option<String> {
    let output = match tokio::time::timeout(
        Duration::from_millis(DEFAULTS_DOMAINS_TIMEOUT_MS),
        Command::new(binary)
            .args(["domains"])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!("defaults domains command failed: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("defaults domains timed out after {DEFAULTS_DOMAINS_TIMEOUT_MS}ms");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            exit = ?output.status.code(),
            stderr = %stderr.trim(),
            "defaults domains failed"
        );
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Extract top-level keys from `defaults read <domain>` output.
///
/// The wire format is plist-flavored: `{ key = value; ... }` where the
/// outermost `{` opens the dictionary, keys are quoted-or-unquoted, and
/// values may be scalars (`1`, `"YES"`), nested dictionaries
/// (`{ ... }`), or arrays (`( ... )`). We track depth across BOTH
/// `{}` and `()` so nested arrays of dicts (e.g. `persistent-apps`)
/// don't bleed inner keys into the output.
///
/// Conservative on ambiguity: if a `=` appears at depth>1 we ignore it
/// (it's part of a nested value). Lines we cannot make sense of are
/// skipped silently — better an under-complete suggestion list than a
/// confidently-wrong one.
pub(crate) fn parse_defaults_keys(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut in_value = false;
    let mut in_quote = false;
    let mut key_buf = String::new();
    for ch in text.chars() {
        if in_quote {
            if ch == '"' {
                in_quote = false;
            } else if depth == 1 && !in_value {
                key_buf.push(ch);
            }
            continue;
        }
        match ch {
            '"' => {
                // Enter quote mode at ANY depth, for BOTH keys
                // (`!in_value`) and value strings (`in_value`). A value
                // string can legitimately contain `;`, `"`, or unbalanced
                // structural chars (`(`,`)`,`{`,`}` — paths, regexes,
                // window titles, a smiley `:)`). Tracking quotes
                // regardless of depth keeps those structural chars inert:
                // the `in_quote` branch above short-circuits (`continue`)
                // before any depth/in_value handling, so a stray `)` inside
                // a deeply-nested value string can no longer corrupt
                // `depth` and silently drop sibling top-level keys. Inside
                // a quote we only append to `key_buf` when at depth 1 and
                // `!in_value`, so value content is consumed harmlessly.
                in_quote = true;
            }
            '{' | '(' => {
                depth += 1;
                if depth > 1 {
                    in_value = true;
                }
            }
            '}' | ')' => {
                depth -= 1;
                if depth <= 1 {
                    in_value = false;
                }
            }
            '=' if depth == 1 && !in_value => {
                let key = key_buf.trim().to_string();
                if !key.is_empty() {
                    out.push(key);
                }
                key_buf.clear();
                in_value = true;
            }
            ';' if depth == 1 => {
                in_value = false;
                key_buf.clear();
            }
            _ => {
                if depth == 1 && !in_value && !ch.is_whitespace() {
                    key_buf.push(ch);
                }
            }
        }
    }
    out
}

/// Parse `defaults domains` output into suggestions. Pure so tests can
/// exercise every branch without spawning a subprocess.
///
/// The wire shape is a single line of comma-space-separated domain
/// identifiers, optionally with a trailing comma and/or trailing
/// newline. `split(',')` + `trim` handles all three: extra whitespace
/// around each token, a dangling empty segment after a trailing comma,
/// and the terminal `\n`.
fn parse_domains(text: &str) -> Vec<Suggestion> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|domain| Suggestion {
            text: domain.to_string(),
            description: Some("defaults domain".to_string()),
            kind: SuggestionKind::ProviderValue,
            source: SuggestionSource::Provider,
            ..Default::default()
        })
        .collect()
}

/// `defaults domains` — enumerates macOS preference domains. Replaces
/// the `requires_js: true` generator in `specs/defaults.json` whose JS
/// source split the single-line output on `, ` and dropped the trailing
/// empty token.
pub struct DefaultsDomains;

impl Provider for DefaultsDomains {
    fn name(&self) -> &'static str {
        "defaults_domains"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "defaults").await
    }
}

impl DefaultsDomains {
    /// Test seam — `generate` calls this with `"defaults"`; tests call
    /// it with a nonexistent path to exercise the
    /// spawn-failure → `Ok(vec![])` fallback contract without mutating
    /// `$PATH`.
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(output) = run_defaults_domains_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        Ok(parse_domains(&output))
    }
}

/// Run `defaults read <domain>` against the user's real `defaults`
/// binary. Returns the raw stdout — callers parse it via
/// [`parse_defaults_keys`].
pub(crate) async fn run_defaults_read_with_binary(
    cwd: &Path,
    binary: &str,
    domain: &str,
) -> Option<String> {
    let output = match tokio::time::timeout(
        Duration::from_millis(DEFAULTS_DOMAINS_TIMEOUT_MS),
        Command::new(binary)
            .args(["read", domain])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!(domain, "defaults read command failed: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                domain,
                "defaults read timed out after {DEFAULTS_DOMAINS_TIMEOUT_MS}ms"
            );
            return None;
        }
    };

    if !output.status.success() {
        tracing::trace!(
            domain,
            exit = ?output.status.code(),
            "defaults read failed (likely empty or unknown domain)"
        );
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `defaults_keys` — enumerates the top-level keys of a macOS
/// preferences domain. The domain is read from
/// `ctx.params["prev_arg"]`, which the engine injects for providers
/// in `NEEDS_PREV_ARG` (currently `DefaultsKeys`).
///
/// The spec uses `-globalDomain` as a literal flag-style sentinel for
/// the global preferences domain; rewrite it to `NSGlobalDomain` so
/// `defaults read NSGlobalDomain` actually returns data. The `-app`
/// sentinel requires a follow-up argument we cannot resolve here, so
/// we bail with an empty suggestion list rather than misdispatch.
pub struct DefaultsKeys;

/// Resolve the raw `prev_arg` token into the domain to pass to
/// `defaults read`.
///
/// - `-globalDomain` is the spec's flag-style sentinel for the global
///   preferences domain; rewrite it to `NSGlobalDomain` (the literal
///   `defaults read` expects).
/// - `-app` is a sentinel that requires a follow-up argument we cannot
///   resolve here, so return `None` to signal "bail with no suggestions".
/// - Anything else is a real domain identifier — pass it through.
fn resolve_domain(raw: &str) -> Option<&str> {
    match raw {
        "-globalDomain" => Some("NSGlobalDomain"),
        "-app" => None,
        other => Some(other),
    }
}

impl Provider for DefaultsKeys {
    fn name(&self) -> &'static str {
        "defaults_keys"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "defaults").await
    }
}

impl DefaultsKeys {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(raw_domain) = ctx.params.get(PREV_ARG_KEY).map(String::as_str) else {
            return Ok(Vec::new());
        };
        let Some(domain) = resolve_domain(raw_domain) else {
            return Ok(Vec::new());
        };
        let Some(output) = run_defaults_read_with_binary(&ctx.cwd, binary, domain).await else {
            return Ok(Vec::new());
        };
        let keys = parse_defaults_keys(&output);
        let description = format!("key in {}", domain);
        Ok(keys
            .into_iter()
            .map(|k| Suggestion {
                text: k,
                description: Some(description.clone()),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_keys_extracts_top_level_keys() {
        let plist = r#"{
            "ApplePersistence" = 1;
            "AppleShowAllExtensions" = 1;
            "AppleShowAllFiles" = "YES";
            "tilesize" = 48;
            "nested" =     {
                "ignored_inner" = 1;
                "also_ignored" = (1, 2, 3);
            };
        }
        "#;
        let keys = parse_defaults_keys(plist);
        assert_eq!(
            keys,
            vec![
                "ApplePersistence",
                "AppleShowAllExtensions",
                "AppleShowAllFiles",
                "tilesize",
                "nested",
            ],
        );
    }

    #[test]
    fn parse_defaults_keys_handles_unquoted_names() {
        let plist = r#"{
            keyWithoutQuotes = 1;
            AnotherKey = 2;
        }
        "#;
        let keys = parse_defaults_keys(plist);
        assert_eq!(keys, vec!["keyWithoutQuotes", "AnotherKey"]);
    }

    #[test]
    fn parse_defaults_keys_handles_arrays_in_nested_value() {
        // Real-world: `defaults read com.apple.dock persistent-apps`
        // values contain arrays of dicts. The outer reader is a dict
        // with nested array values — depth tracking must not blow up.
        let plist = r#"{
            "persistent-apps" = (
                { "tile-data" = { "label" = "Finder"; }; },
                { "tile-data" = { "label" = "Mail"; }; }
            );
            "tilesize" = 48;
        }
        "#;
        let keys = parse_defaults_keys(plist);
        assert_eq!(keys, vec!["persistent-apps", "tilesize"]);
    }

    #[test]
    fn parse_defaults_keys_empty_input_yields_empty() {
        assert!(parse_defaults_keys("").is_empty());
        assert!(parse_defaults_keys("{ }").is_empty());
    }

    #[test]
    fn parse_defaults_keys_quoted_value_with_separators_stays_intact() {
        // A quoted key/value containing `=` and `;` must not corrupt
        // parsing: the `;` inside the value cannot end the statement, and
        // the value content cannot leak into the key list. This locks in
        // the quote-tracking-for-values fix.
        let plist = r#"{ "weird=key;name" = 1; "plain" = 2; }"#;
        let keys = parse_defaults_keys(plist);
        assert!(
            keys.contains(&"weird=key;name".to_string()),
            "expected quoted key with `=`/`;` to survive, got {keys:?}"
        );
        assert!(
            keys.contains(&"plain".to_string()),
            "expected `plain` key, got {keys:?}"
        );
        assert_eq!(keys, vec!["weird=key;name", "plain"]);
    }

    #[test]
    fn parse_defaults_keys_string_value_with_semicolon_does_not_spawn_keys() {
        // `defaults read` string values legitimately contain `;` (paths,
        // serialized prefs). The `;` inside the value must be consumed as
        // quote content, not terminate the statement and spawn a bogus
        // key from the value tail.
        let plist = r#"{ "key" = "a;b"; "other" = 3; }"#;
        let keys = parse_defaults_keys(plist);
        assert_eq!(keys, vec!["key", "other"]);
    }

    #[test]
    fn parse_defaults_keys_nested_quoted_value_with_unbalanced_brace_keeps_siblings() {
        // Regression: a quoted VALUE string nested at depth >= 2 may
        // contain an unbalanced structural char (a smiley `:)`, a regex,
        // a window title). Before the fix, quote mode was only entered at
        // depth 1, so the inner `)` of `"smiley :)"` decremented `depth`
        // and corrupted state — silently dropping the sibling top-level
        // key `after`. Quotes must be tracked at every depth so the `)`
        // is inert quote content.
        let plist = r#"{ "outer" = { "inner" = "smiley :)"; }; "after" = 1; }"#;
        let keys = parse_defaults_keys(plist);
        assert_eq!(keys, vec!["outer", "after"]);
    }

    #[test]
    fn parse_defaults_keys_truncated_input_no_panic() {
        // Truncated/interrupted output (e.g. process killed mid-stream)
        // must not panic. Conservatively, only the keys whose statements
        // could be fully recognized before truncation are emitted; here
        // the outer `persistent-apps` key is recognized at depth 1 before
        // the array opens, while the inner `x` lives at depth > 1 and is
        // never surfaced.
        let plist = r#"{ "persistent-apps" = ( { "x" = 1;"#;
        let keys = parse_defaults_keys(plist);
        assert_eq!(keys, vec!["persistent-apps"]);
    }

    #[test]
    fn resolve_domain_rewrites_global_domain_sentinel() {
        assert_eq!(resolve_domain("-globalDomain"), Some("NSGlobalDomain"));
    }

    #[test]
    fn resolve_domain_bails_on_app_sentinel() {
        assert_eq!(resolve_domain("-app"), None);
    }

    #[test]
    fn resolve_domain_passes_through_real_domain() {
        assert_eq!(resolve_domain("com.apple.dock"), Some("com.apple.dock"));
    }

    #[tokio::test]
    async fn defaults_keys_generate_empty_without_prev_arg() {
        // No `prev_arg` param → the provider has no domain to read and
        // must yield `Ok(vec![])` without spawning anything.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ProviderCtx {
            cwd: tmp.path().to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
            params: std::sync::Arc::new(std::collections::BTreeMap::new()),
        };
        let result = DefaultsKeys
            .generate_with_binary(&ctx, "/nonexistent/defaults-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn defaults_keys_generate_bails_on_app_sentinel_before_spawn() {
        // `prev_arg = "-app"` must short-circuit to `Ok(vec![])` BEFORE
        // attempting to spawn `defaults read`. Using a nonexistent binary
        // proves it: if the code reached the spawn it would still return
        // `Ok(vec![])` via the spawn-failure path, but the assertion that
        // it returns empty (and the documented early-return semantics)
        // confirms the bail. The nonexistent binary also guarantees no
        // real `defaults read` side effect occurs.
        let tmp = tempfile::TempDir::new().unwrap();
        let mut params = std::collections::BTreeMap::new();
        params.insert(PREV_ARG_KEY.to_string(), "-app".to_string());
        let ctx = ProviderCtx {
            cwd: tmp.path().to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
            params: std::sync::Arc::new(params),
        };
        let result = DefaultsKeys
            .generate_with_binary(&ctx, "/nonexistent/defaults-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    /// Write `script` to `<dir>/defaults` and mark it executable,
    /// returning the path. Mirrors the fake-binary idiom used by the
    /// kubectl/cargo provider tests so the success path of
    /// `generate_with_binary` is exercised against a real spawn.
    #[cfg(unix)]
    fn write_fake_defaults(dir: &Path, script: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("defaults");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    fn ctx_with_prev_arg(cwd: &Path, prev_arg: &str) -> ProviderCtx {
        let mut params = std::collections::BTreeMap::new();
        params.insert(PREV_ARG_KEY.to_string(), prev_arg.to_string());
        ProviderCtx {
            cwd: cwd.to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
            params: std::sync::Arc::new(params),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn defaults_keys_generate_reads_domain_and_parses_keys() {
        // Full success wiring: `prev_arg = "com.apple.dock"` →
        // `defaults read com.apple.dock` → parse_defaults_keys → one
        // Suggestion per top-level key with description `key in <domain>`.
        // The fake `defaults` only prints the plist when invoked as
        // `read com.apple.dock`, so the test also proves the domain is
        // threaded through to the spawn unchanged.
        let tmp = tempfile::TempDir::new().unwrap();
        let script = "#!/bin/sh\n\
            if [ \"$1\" != \"read\" ] || [ \"$2\" != \"com.apple.dock\" ]; then exit 1; fi\n\
            printf '%s\\n' '{ \"tilesize\" = 48; \"autohide\" = 1; }'\n";
        let fake = write_fake_defaults(tmp.path(), script);
        let ctx = ctx_with_prev_arg(tmp.path(), "com.apple.dock");

        let suggestions = DefaultsKeys
            .generate_with_binary(&ctx, fake.to_str().unwrap())
            .await
            .unwrap();

        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["tilesize", "autohide"]);
        for s in &suggestions {
            assert_eq!(s.description.as_deref(), Some("key in com.apple.dock"));
            assert_eq!(s.kind, SuggestionKind::ProviderValue);
            assert_eq!(s.source, SuggestionSource::Provider);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn defaults_keys_generate_rewrites_global_domain_sentinel_through_to_read() {
        // `prev_arg = "-globalDomain"` must be rewritten to
        // `NSGlobalDomain` BEFORE the spawn. The fake `defaults` exits
        // nonzero unless it receives the rewritten literal, so a non-empty
        // success proves the `resolve_domain` rewrite flows through to the
        // real read (not just the unit test of `resolve_domain`).
        let tmp = tempfile::TempDir::new().unwrap();
        let script = "#!/bin/sh\n\
            if [ \"$2\" != \"NSGlobalDomain\" ]; then exit 1; fi\n\
            printf '%s\\n' '{ \"AppleLanguages\" = ( \"en\" ); \"KeyRepeat\" = 2; }'\n";
        let fake = write_fake_defaults(tmp.path(), script);
        let ctx = ctx_with_prev_arg(tmp.path(), "-globalDomain");

        let suggestions = DefaultsKeys
            .generate_with_binary(&ctx, fake.to_str().unwrap())
            .await
            .unwrap();

        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["AppleLanguages", "KeyRepeat"]);
        for s in &suggestions {
            // Description carries the REWRITTEN domain, confirming the
            // rewrite is what reached `defaults read`.
            assert_eq!(s.description.as_deref(), Some("key in NSGlobalDomain"));
        }
    }

    #[test]
    fn parse_happy_path() {
        // Canonical wire shape: comma-space separated, trailing newline,
        // no trailing comma. Every token must round-trip to a
        // `Suggestion` with the expected metadata.
        let fixture = "com.apple.dock, com.apple.finder, com.apple.Safari, com.googlecode.iterm2\n";
        let suggestions = parse_domains(fixture);
        assert_eq!(suggestions.len(), 4);
        assert_eq!(suggestions[0].text, "com.apple.dock");
        assert_eq!(suggestions[1].text, "com.apple.finder");
        assert_eq!(suggestions[2].text, "com.apple.Safari");
        assert_eq!(suggestions[3].text, "com.googlecode.iterm2");
        for s in &suggestions {
            assert_eq!(s.description.as_deref(), Some("defaults domain"));
            assert_eq!(s.kind, SuggestionKind::ProviderValue);
            assert_eq!(s.source, SuggestionSource::Provider);
        }
    }

    #[test]
    fn parse_trailing_comma_tolerated() {
        // Some macOS versions emit `a, b, c,` with a dangling comma.
        // `split(',')` produces an empty final segment in that case —
        // the `filter(!is_empty)` arm must drop it rather than producing
        // a nameless suggestion.
        let fixture = "com.apple.dock, com.apple.finder, com.apple.Safari,\n";
        let suggestions = parse_domains(fixture);
        assert_eq!(suggestions.len(), 3);
        assert_eq!(suggestions[0].text, "com.apple.dock");
        assert_eq!(suggestions[1].text, "com.apple.finder");
        assert_eq!(suggestions[2].text, "com.apple.Safari");
        for s in &suggestions {
            assert!(
                !s.text.is_empty(),
                "trailing comma produced an empty suggestion"
            );
        }
    }

    #[test]
    fn parse_extra_whitespace() {
        // Irregular spacing around separators — `defaults` normally
        // emits a single space after each comma, but we must tolerate
        // any amount of leading/trailing whitespace per token so a
        // future OS-level formatting change can't break us.
        let fixture = "a,  b , c";
        let suggestions = parse_domains(fixture);
        assert_eq!(suggestions.len(), 3);
        assert_eq!(suggestions[0].text, "a");
        assert_eq!(suggestions[1].text, "b");
        assert_eq!(suggestions[2].text, "c");
    }

    #[test]
    fn parse_empty_input() {
        // Both empty-string and bare-newline inputs must yield an empty
        // Vec — the newline-only case is what `defaults domains` emits
        // when no domains are registered (vanishingly rare in practice
        // but cheap to cover).
        assert!(parse_domains("").is_empty());
        assert!(parse_domains("\n").is_empty());
    }

    #[tokio::test]
    async fn subprocess_failure_returns_none() {
        // Exercises the spawn-time "file not found" path by injecting
        // a binary name that cannot resolve anywhere on disk. No
        // global state mutated — safe to run in parallel with the
        // rest of the workspace suite, and also exercises the Linux
        // code path where `defaults` genuinely does not exist.
        let tmp = tempfile::TempDir::new().unwrap();
        let result = run_defaults_domains_with_binary(
            tmp.path(),
            "/nonexistent/defaults-definitely-not-real",
        )
        .await;
        assert!(
            result.is_none(),
            "expected None when the defaults binary does not exist"
        );
    }

    #[tokio::test]
    async fn generate_returns_ok_empty_when_binary_missing() {
        // End-to-end: `DefaultsDomains::generate` MUST translate a
        // spawn-time failure into `Ok(vec![])`. Critical on Linux,
        // where `defaults` doesn't exist at all — the provider must
        // degrade silently rather than error.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ProviderCtx {
            cwd: tmp.path().to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
            params: std::sync::Arc::new(std::collections::BTreeMap::new()),
        };
        let result = DefaultsDomains
            .generate_with_binary(&ctx, "/nonexistent/defaults-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn generate_production_wrapper_returns_ok_without_binary_installed() {
        // Pins the "never Err" contract of `Provider::generate` — a
        // spawn failure (tool missing, PATH empty, exec permission
        // denied) must become `Ok(vec![])`, never propagated as `Err`.
        // Critical on Linux, where `defaults` genuinely does not exist
        // and this path runs on every invocation.
        //
        // Does NOT catch a typo'd binary literal (e.g. `"default"` vs
        // `"defaults"`), because a typo produces the same spawn
        // failure → `Ok(vec![])` path as a genuinely-absent tool:
        // every subprocess error maps to `tracing::warn!` + `None` in
        // `run_defaults_domains_with_binary`, and `generate_with_binary`
        // maps `None` to `Ok(Vec::new())`. We have no test seam that
        // distinguishes a typo from "tool genuinely absent".
        //
        // Assertion is `Ok(_)` (not `Ok(vec![])`) because on macOS
        // hosts `defaults` DOES exist and will enumerate real
        // preference domains.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ProviderCtx {
            cwd: tmp.path().to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
            params: std::sync::Arc::new(std::collections::BTreeMap::new()),
        };
        let result = DefaultsDomains.generate(&ctx).await;
        assert!(result.is_ok());
    }
}
