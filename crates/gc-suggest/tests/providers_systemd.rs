pub use gc_suggest::providers::{Provider, ProviderCtx};

mod types {
    pub use gc_suggest::types::*;
}

mod providers {
    pub mod util {
        include!("../src/providers/util.rs");
    }
}

mod util {
    pub(crate) use crate::providers::util::*;
}

mod version_probe {
    include!("../src/providers/version_probe.rs");
}

#[path = "../src/providers/systemd_units.rs"]
mod systemd_units;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use systemd_units::{
    parse_systemd_json_units, parse_systemd_text_units, systemd_units_to_suggestions,
    SystemdActiveUnits, SystemdFilter, SystemdUnits, SystemdUserUnits,
};

fn ctx() -> ProviderCtx {
    ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    }
}

#[test]
fn provider_names_match_spec_type_strings() {
    assert_eq!(SystemdUnits.name(), "systemd_units");
    assert_eq!(SystemdUserUnits.name(), "systemd_user_units");
    assert_eq!(SystemdActiveUnits.name(), "systemd_active_units");
}

#[test]
fn parses_recorded_systemd_json_output() {
    let units = parse_systemd_json_units(
        r#"[
            {
                "unit": "ssh.service",
                "load": "loaded",
                "active": "active",
                "sub": "running",
                "description": "OpenSSH server daemon"
            },
            {
                "unit": "db.service",
                "load": "loaded",
                "active": "failed",
                "sub": "failed",
                "description": "Example DB"
            }
        ]"#,
    )
    .expect("JSON output should parse");

    let suggestions = systemd_units_to_suggestions(units, SystemdFilter::All);
    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["db.service", "ssh.service"]);
    assert_eq!(
        suggestions[0].description.as_deref(),
        Some("failed - Example DB")
    );
    assert_eq!(
        suggestions[1].description.as_deref(),
        Some("active - OpenSSH server daemon")
    );
    for suggestion in &suggestions {
        assert_eq!(suggestion.kind, gc_suggest::SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, gc_suggest::SuggestionSource::Provider);
    }
}

#[test]
fn parses_empty_systemd_outputs_as_empty() {
    let units = parse_systemd_json_units("[]").expect("empty JSON list should parse");
    assert!(units.is_empty());
    assert!(parse_systemd_text_units("").is_empty());
    assert!(parse_systemd_text_units("\n\n").is_empty());
}

#[test]
fn malformed_json_returns_none_and_text_parser_drops_bad_rows() {
    assert!(parse_systemd_json_units("not json").is_none());
    assert!(parse_systemd_json_units("{").is_none());

    let units = parse_systemd_text_units(
        "not-enough-columns\n\
         also.bad loaded active\n\
         \tloaded active running missing-unit\n",
    );
    assert!(units.is_empty());
}

#[test]
fn parses_no_legend_text_fallback_output() {
    let units = parse_systemd_text_units(
        "ssh.service loaded active running OpenSSH server daemon\n\
         dbus.service loaded inactive dead D-Bus System Message Bus\n",
    );

    let suggestions = systemd_units_to_suggestions(units, SystemdFilter::All);
    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["dbus.service", "ssh.service"]);
    assert_eq!(
        suggestions[0].description.as_deref(),
        Some("inactive - D-Bus System Message Bus")
    );
    assert_eq!(
        suggestions[1].description.as_deref(),
        Some("active - OpenSSH server daemon")
    );
}

#[test]
fn active_units_filter_removes_inactive_rows() {
    let units = parse_systemd_json_units(
        r#"[
            {"unit":"active.service","active":"active","description":"Active"},
            {"unit":"inactive.service","active":"inactive","description":"Inactive"},
            {"unit":"failed.service","active":"failed","description":"Failed"}
        ]"#,
    )
    .expect("JSON output should parse");

    let suggestions = systemd_units_to_suggestions(units, SystemdFilter::ActiveOnly);
    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["active.service"]);
}

#[tokio::test]
async fn generate_returns_empty_when_systemctl_binary_is_missing() {
    let result = SystemdUnits
        .generate_with_binary(&ctx(), "/nonexistent/systemctl-for-test")
        .await;

    assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
}
