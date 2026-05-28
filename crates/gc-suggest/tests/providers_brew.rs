use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

mod types {
    pub use gc_suggest::types::*;
}

mod providers {
    pub use gc_suggest::providers::{Provider, ProviderCtx};

    pub mod util {
        include!("../src/providers/util.rs");
    }

    pub mod version_probe {
        include!("../src/providers/version_probe.rs");
    }

    pub mod brew {
        include!("../src/providers/brew.rs");
    }
}

use gc_suggest::providers::{Provider, ProviderCtx};
use gc_suggest::types::{SuggestionKind, SuggestionSource};
use providers::brew::{
    brew_search_plan, parse_casks_installed_output, parse_casks_searchable_output,
    parse_formulae_installed_output, parse_formulae_searchable_output,
    parse_packages_searchable_output, run_brew_with_binary, set_brew_search_cap,
    BrewCasksInstalled, BrewCasksSearchable, BrewFormulaeInstalled, BrewFormulaeSearchable,
    BrewPackagesSearchable, DEFAULT_BREW_SEARCH_CAP,
};

fn ctx_for(cwd: &Path) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    }
}

#[test]
fn provider_names_match_spec_type_strings() {
    assert_eq!(BrewFormulaeInstalled.name(), "brew_formulae_installed");
    assert_eq!(BrewCasksInstalled.name(), "brew_casks_installed");
    assert_eq!(BrewFormulaeSearchable.name(), "brew_formulae_searchable");
    assert_eq!(BrewCasksSearchable.name(), "brew_casks_searchable");
    assert_eq!(BrewPackagesSearchable.name(), "brew_packages_searchable");
}

#[test]
fn installed_formulae_parse_recorded_brew_list_output() {
    let fixture = "autoconf\nopenssl@3\nripgrep\nzstd\n";
    let suggestions = parse_formulae_installed_output(fixture);

    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["autoconf", "openssl@3", "ripgrep", "zstd"]);
    for suggestion in &suggestions {
        assert_eq!(
            suggestion.description.as_deref(),
            Some("installed brew formula")
        );
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn installed_casks_parse_recorded_brew_list_output() {
    let fixture = "ghostty\ngoogle-chrome\nvisual-studio-code\n";
    let suggestions = parse_casks_installed_output(fixture);

    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["ghostty", "google-chrome", "visual-studio-code"]
    );
    for suggestion in &suggestions {
        assert_eq!(
            suggestion.description.as_deref(),
            Some("installed brew cask")
        );
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn searchable_formulae_parse_recorded_brew_search_output() {
    let fixture = "\
==> Formulae
a2ps                         abcm2ps                      ack
openssl@3

==> Casks
1password                    docker
";
    let suggestions = parse_formulae_searchable_output(fixture, DEFAULT_BREW_SEARCH_CAP);

    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["a2ps", "abcm2ps", "ack", "openssl@3"]);
    for suggestion in &suggestions {
        assert_eq!(suggestion.description.as_deref(), Some("brew formula"));
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn parsers_return_empty_for_empty_output() {
    assert!(parse_formulae_installed_output("").is_empty());
    assert!(parse_formulae_installed_output("\n\n").is_empty());
    assert!(parse_casks_installed_output("").is_empty());
    assert!(parse_casks_installed_output("\n").is_empty());
    assert!(parse_formulae_searchable_output("", DEFAULT_BREW_SEARCH_CAP).is_empty());
    assert!(parse_formulae_searchable_output("\n\n", DEFAULT_BREW_SEARCH_CAP).is_empty());
}

#[test]
fn searchable_formulae_respect_default_cap() {
    let fixture = (0..1005)
        .map(|i| format!("formula-{i}"))
        .collect::<Vec<_>>()
        .join("\n");

    let suggestions = parse_formulae_searchable_output(&fixture, DEFAULT_BREW_SEARCH_CAP);

    assert_eq!(DEFAULT_BREW_SEARCH_CAP, 1000);
    assert_eq!(suggestions.len(), 1000);
    assert_eq!(suggestions[0].text, "formula-0");
    assert_eq!(suggestions[999].text, "formula-999");
}

#[test]
fn set_brew_search_cap_normalises_zero_input() {
    // The brew search cap is a process-global atomic. Setter is called
    // once at engine startup with the experimental config value; this
    // test pins the "0 is normalised to 1" contract that prevents a
    // misconfigured cap from suppressing every suggestion. We do not
    // observe the global value back here because that would race with
    // any sibling test that uses the live BrewFormulaeSearchable
    // dispatch — the setter API alone is what we exercise.
    set_brew_search_cap(0);
    set_brew_search_cap(DEFAULT_BREW_SEARCH_CAP);
}

#[test]
fn brew_search_plan_forwards_typed_query_and_skips_cap() {
    let (args, cap) = brew_search_plan("rust", &[], DEFAULT_BREW_SEARCH_CAP);
    assert_eq!(args, vec!["search", "rust"]);
    assert_eq!(
        cap,
        usize::MAX,
        "typed-query search must not be cap-clipped; only empty-query exploration is"
    );
}

#[test]
fn brew_search_plan_uses_empty_arg_and_cap_for_empty_query() {
    let (args, cap) = brew_search_plan("", &[], 7);
    assert_eq!(args, vec!["search", ""]);
    assert_eq!(cap, 7);
}

#[test]
fn brew_search_plan_splices_flag_prefix_for_cask_path() {
    // The cask provider routes through brew_search_plan with a --cask
    // flag prefix; the planner must splice it between `search` and the
    // query (or the empty exploration arg). A regression that dropped
    // --cask or appended it after the token would surface here.
    let (empty_args, empty_cap) = brew_search_plan("", &["--cask"], DEFAULT_BREW_SEARCH_CAP);
    assert_eq!(
        empty_args,
        vec!["search", "--cask", ""],
        "empty-query cask search must be `search --cask \"\"`"
    );
    assert_eq!(
        empty_cap, DEFAULT_BREW_SEARCH_CAP,
        "empty-query cask exploration must keep the cap"
    );

    let (typed_args, typed_cap) = brew_search_plan("firefox", &["--cask"], DEFAULT_BREW_SEARCH_CAP);
    assert_eq!(
        typed_args,
        vec!["search", "--cask", "firefox"],
        "typed cask search must forward the token after --cask"
    );
    assert_eq!(
        typed_cap,
        usize::MAX,
        "typed cask search must drop the cap like the formulae path"
    );
}

#[test]
fn brew_search_plan_pins_whitespace_and_dash_query_contract() {
    // brew_search_plan keys off `query.is_empty()` only, so any
    // non-empty string — whitespace-only or a leading-dash token — is
    // treated as a typed query: forwarded verbatim with the cap dropped.
    // This pins the boundary so a future change to the empty-check is
    // regression-protected. (We do NOT trim or reject these here; the
    // upstream buffer/trigger layer decides when a search even fires.)
    let (space_args, space_cap) = brew_search_plan(" ", &[], DEFAULT_BREW_SEARCH_CAP);
    assert_eq!(
        space_args,
        vec!["search", " "],
        "whitespace-only query is non-empty and forwarded verbatim"
    );
    assert_eq!(space_cap, usize::MAX, "non-empty query drops the cap");

    let (tab_args, _tab_cap) = brew_search_plan("\t", &[], DEFAULT_BREW_SEARCH_CAP);
    assert_eq!(tab_args, vec!["search", "\t"]);

    let (dash_args, dash_cap) = brew_search_plan("-", &[], DEFAULT_BREW_SEARCH_CAP);
    assert_eq!(
        dash_args,
        vec!["search", "-"],
        "leading-dash query is non-empty and forwarded verbatim"
    );
    assert_eq!(dash_cap, usize::MAX);

    let (dashx_args, _) = brew_search_plan("-x", &[], DEFAULT_BREW_SEARCH_CAP);
    assert_eq!(dashx_args, vec!["search", "-x"]);
}

#[test]
fn parse_casks_searchable_handles_modern_and_legacy_headers() {
    // Homebrew 4.x style: `==> Casks` header marks the cask section.
    // The leading `==> Formulae` header SUPPRESSES formula1; only the
    // tokens after `==> Casks` survive.
    let modern = "\
==> Formulae
formula1
==> Casks
cask1
cask2
";
    let parsed = parse_casks_searchable_output(modern, DEFAULT_BREW_SEARCH_CAP);
    let texts: Vec<&str> = parsed.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["cask1", "cask2"]);
    for s in &parsed {
        assert_eq!(s.description.as_deref(), Some("brew cask"));
        assert_eq!(s.kind, SuggestionKind::ProviderValue);
        assert_eq!(s.source, SuggestionSource::Provider);
    }

    // Homebrew 2.x legacy style: `Casks:` colon header.
    let legacy = "\
Formulae:
formula1
Casks:
cask1
cask2
";
    let parsed = parse_casks_searchable_output(legacy, DEFAULT_BREW_SEARCH_CAP);
    let texts: Vec<&str> = parsed.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["cask1", "cask2"]);
}

#[test]
fn parse_casks_searchable_emits_header_less_modern_cask_output() {
    // CRITICAL regression guard for F1: `brew search --cask <q>` on
    // modern Homebrew (5.x) prints a BARE, header-less token list with
    // ZERO `==>` lines. The parser must default `in_casks` ON and treat
    // every line as a cask — the previous default-OFF behaviour returned
    // an empty Vec and made BrewCasksSearchable non-functional.
    let header_less = "dbvisualizer\nvisual-studio\n";
    let parsed = parse_casks_searchable_output(header_less, DEFAULT_BREW_SEARCH_CAP);
    let texts: Vec<&str> = parsed.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["dbvisualizer", "visual-studio"],
        "header-less --cask output must yield every token as a cask"
    );
    assert!(
        !parsed.is_empty(),
        "header-less cask output must be NON-EMPTY (F1 regression guard)"
    );
}

#[test]
fn parse_casks_searchable_excludes_formulae_when_header_follows_cask_block() {
    // F7: a `==> Formulae` header appearing AFTER the cask block must
    // switch emission OFF so formula tokens are not mis-projected as
    // casks. Mirrors the formulae parser's section-suppression.
    let casks_then_formulae = "==> Casks\ncask1\ncask2\n==> Formulae\nformula1\n";
    let parsed = parse_casks_searchable_output(casks_then_formulae, DEFAULT_BREW_SEARCH_CAP);
    let texts: Vec<&str> = parsed.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["cask1", "cask2"],
        "formula tokens after a `==> Formulae` header must be excluded"
    );

    // Legacy `Formulae:` colon header must reset emission off too.
    let legacy_reset = "Casks:\ncask1\nFormulae:\nformula1\n";
    let parsed = parse_casks_searchable_output(legacy_reset, DEFAULT_BREW_SEARCH_CAP);
    let texts: Vec<&str> = parsed.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["cask1"]);
}

#[test]
fn parse_casks_searchable_returns_empty_when_only_formulae_section() {
    // With a `==> Formulae` header and no cask section, emission stays
    // suppressed and the result is empty. (Empty input is trivially
    // empty too.)
    let formulae_only = "==> Formulae\nfoo\nbar\n";
    assert!(parse_casks_searchable_output(formulae_only, DEFAULT_BREW_SEARCH_CAP).is_empty());
    assert!(parse_casks_searchable_output("", DEFAULT_BREW_SEARCH_CAP).is_empty());
}

#[test]
fn parse_casks_searchable_respects_cap() {
    // F2: the empty-query exploration path (`brew search --cask ""`)
    // returns ~7.7k header-less lines; the cap must clip emission.
    let fixture = (0..50)
        .map(|i| format!("cask-{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = parse_casks_searchable_output(&fixture, 10);
    assert_eq!(parsed.len(), 10);
    assert_eq!(parsed[0].text, "cask-0");
    assert_eq!(parsed[9].text, "cask-9");
}

#[test]
fn parse_packages_searchable_emits_formulae_and_casks() {
    let input = "==> Formulae\nfoo\nbaz\n==> Casks\nbar\n";
    let parsed = parse_packages_searchable_output(input, DEFAULT_BREW_SEARCH_CAP);
    let texts: Vec<&str> = parsed.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["foo", "baz", "bar"]);
    for s in &parsed {
        assert_eq!(s.description.as_deref(), Some("brew formula or cask"));
        assert_eq!(s.kind, SuggestionKind::ProviderValue);
        assert_eq!(s.source, SuggestionSource::Provider);
    }
}

#[test]
fn parse_packages_searchable_handles_legacy_headers() {
    let legacy = "Formulae:\nfoo\nCasks:\nbar\n";
    let parsed = parse_packages_searchable_output(legacy, DEFAULT_BREW_SEARCH_CAP);
    let texts: Vec<&str> = parsed.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["foo", "bar"]);
}

#[test]
fn parse_packages_searchable_respects_cap() {
    // F2: `brew search ""` returns ~16k lines (formulae + casks union)
    // on modern Homebrew; the cap must clip emission exactly as it does
    // for the formulae-only path.
    let fixture = (0..50)
        .map(|i| format!("pkg-{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = parse_packages_searchable_output(&fixture, 10);
    assert_eq!(parsed.len(), 10);
    assert_eq!(parsed[0].text, "pkg-0");
    assert_eq!(parsed[9].text, "pkg-9");
}

#[tokio::test]
async fn run_brew_missing_binary_returns_none() {
    let tmp = tempfile::TempDir::new().unwrap();

    let output = run_brew_with_binary(
        tmp.path(),
        "/nonexistent/brew-definitely-not-real",
        &["list", "--formula"],
    )
    .await;

    assert!(output.is_none());
}

#[tokio::test]
async fn providers_return_ok_empty_when_brew_binary_is_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = ctx_for(tmp.path());
    let missing = "/nonexistent/brew-for-provider-tests";

    let formulae = BrewFormulaeInstalled
        .generate_with_binary(&ctx, missing)
        .await
        .unwrap();
    let casks = BrewCasksInstalled
        .generate_with_binary(&ctx, missing)
        .await
        .unwrap();
    let searchable = BrewFormulaeSearchable
        .generate_with_binary(&ctx, missing)
        .await
        .unwrap();
    let cask_search = BrewCasksSearchable
        .generate_with_binary(&ctx, missing)
        .await
        .unwrap();
    let pkg_search = BrewPackagesSearchable
        .generate_with_binary(&ctx, missing)
        .await
        .unwrap();

    assert!(formulae.is_empty());
    assert!(casks.is_empty());
    assert!(searchable.is_empty());
    assert!(cask_search.is_empty());
    assert!(pkg_search.is_empty());
}

// ---------- end-to-end provider wiring against a fake `brew` (F4) ----------
//
// The unit tests above exercise the parsers and the argv planner in
// isolation. These cover the wiring the planner+parser tests cannot:
// generate_with_binary -> reads ctx.current_token -> run_brew_with_binary
// -> parses stdout. A fake `brew` script (a) answers `--version` with a
// >=2.0 version so brew_is_supported passes, (b) records the search argv
// to a sibling file, and (c) prints canned output. The fake's path is
// unique per test (its own tempdir), which also keeps the process-global
// version-probe cache keyed distinctly per test.

/// Write an executable fake `brew` into `dir`, returning the binary path.
/// The script answers `--version` (without recording), and for any other
/// invocation records its full argv (one arg per line) to
/// `<dir>/argv.log` then prints `canned_stdout`.
fn write_fake_brew(dir: &Path, canned_stdout: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = dir.join("brew");
    let log = dir.join("argv.log");
    // `$0` lets the script find its own directory so the record path does
    // not depend on cwd (the providers spawn brew with ctx.cwd as cwd).
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then\n\
           echo 'Homebrew 4.2.10'\n\
           exit 0\n\
         fi\n\
         : > '{log}'\n\
         for a in \"$@\"; do echo \"$a\" >> '{log}'; done\n\
         cat <<'EOF'\n\
         {canned}EOF\n",
        log = log.display(),
        canned = canned_stdout,
    );
    std::fs::write(&bin, script).unwrap();
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();
    bin
}

fn read_argv_log(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("argv.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn ctx_with_token(cwd: &Path, token: &str) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: token.to_string(),
        params: Arc::new(BTreeMap::new()),
    }
}

#[tokio::test]
async fn formulae_searchable_forwards_token_and_parses_output() {
    let tmp = tempfile::TempDir::new().unwrap();
    let canned = "==> Formulae\nrust\nripgrep\n==> Casks\nfirefox\n";
    let bin = write_fake_brew(tmp.path(), canned);
    let ctx = ctx_with_token(tmp.path(), "rust");

    let out = BrewFormulaeSearchable
        .generate_with_binary(&ctx, bin.to_str().unwrap())
        .await
        .unwrap();

    let argv = read_argv_log(tmp.path());
    assert_eq!(
        argv,
        vec!["search", "rust"],
        "BrewFormulaeSearchable must forward ctx.current_token to `brew search <q>`"
    );
    let texts: Vec<&str> = out.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["rust", "ripgrep"],
        "only the Formulae section should be projected"
    );
}

#[tokio::test]
async fn casks_searchable_forwards_token_with_cask_flag_and_parses_output() {
    let tmp = tempfile::TempDir::new().unwrap();
    // `brew search --cask <q>` on modern Homebrew prints a header-less list.
    let canned = "firefox\nfirefox-developer-edition\n";
    let bin = write_fake_brew(tmp.path(), canned);
    let ctx = ctx_with_token(tmp.path(), "firefox");

    let out = BrewCasksSearchable
        .generate_with_binary(&ctx, bin.to_str().unwrap())
        .await
        .unwrap();

    let argv = read_argv_log(tmp.path());
    assert_eq!(
        argv,
        vec!["search", "--cask", "firefox"],
        "BrewCasksSearchable must invoke `brew search --cask <q>` with the forwarded token"
    );
    let texts: Vec<&str> = out.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["firefox", "firefox-developer-edition"],
        "header-less --cask output must parse end-to-end (F1 wiring)"
    );
}

#[tokio::test]
async fn packages_searchable_forwards_token_and_parses_union() {
    let tmp = tempfile::TempDir::new().unwrap();
    let canned = "==> Formulae\nrust\n==> Casks\nfirefox\n";
    let bin = write_fake_brew(tmp.path(), canned);
    let ctx = ctx_with_token(tmp.path(), "rust");

    let out = BrewPackagesSearchable
        .generate_with_binary(&ctx, bin.to_str().unwrap())
        .await
        .unwrap();

    let argv = read_argv_log(tmp.path());
    assert_eq!(
        argv,
        vec!["search", "rust"],
        "BrewPackagesSearchable must forward ctx.current_token to `brew search <q>`"
    );
    let texts: Vec<&str> = out.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["rust", "firefox"],
        "packages search projects both formulae and casks"
    );
}

#[tokio::test]
async fn run_brew_demotes_no_match_failure_to_none() {
    use std::os::unix::fs::PermissionsExt;

    // F5: a typed query that matches nothing makes modern brew exit 1
    // with `Error: No formulae or casks found ...` on stderr. That is an
    // expected no-match, not a command failure — run_brew_with_binary
    // must return None (parsed as empty) without escalating to a warn.
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = tmp.path().join("brew");
    let script = "#!/bin/sh\n\
                  echo 'Error: No formulae or casks found for \"zzzznope\".' 1>&2\n\
                  exit 1\n";
    std::fs::write(&bin, script).unwrap();
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();

    let out =
        run_brew_with_binary(tmp.path(), bin.to_str().unwrap(), &["search", "zzzznope"]).await;
    assert!(
        out.is_none(),
        "a brew no-match exit must map to None (empty result), not surface output"
    );
}
