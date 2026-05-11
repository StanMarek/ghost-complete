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
    parse_casks_installed_output, parse_formulae_installed_output,
    parse_formulae_searchable_output, run_brew_with_binary, BrewCasksInstalled,
    BrewFormulaeInstalled, BrewFormulaeSearchable, DEFAULT_BREW_SEARCH_CAP,
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

    assert!(formulae.is_empty());
    assert!(casks.is_empty());
    assert!(searchable.is_empty());
}
