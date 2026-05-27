//! Integration tests for `macos_applications` + `macos_bundle_identifiers`.
//! The async providers are exercised by lib-level subprocess tests in
//! `crates/gc-suggest/src/providers/macos_apps.rs`; this file pins the
//! pure parsers via the crate's public API so other crates depending
//! on `gc_suggest::providers::macos_apps::*` see the same contract.

use gc_suggest::providers::macos_apps::{parse_applications, parse_bundle_identifiers};

#[test]
fn parse_applications_real_world_paths() {
    let mdfind = "\
/Applications/Safari.app
/Applications/Terminal.app
/System/Applications/Utilities/Activity Monitor.app
/Users/stan/Applications/Visual Studio Code.app
";
    let parsed = parse_applications(mdfind);
    let names: Vec<&str> = parsed.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Safari",
            "Terminal",
            "Activity Monitor",
            "Visual Studio Code",
        ]
    );
    let paths: Vec<&str> = parsed.iter().map(|(_, p)| p.as_str()).collect();
    assert_eq!(paths[0], "/Applications/Safari.app");
    assert_eq!(
        paths[2],
        "/System/Applications/Utilities/Activity Monitor.app"
    );
}

#[test]
fn parse_bundle_identifiers_dedupe_and_filter_null() {
    let mdls = "\
kMDItemCFBundleIdentifier = \"com.apple.Safari\"
kMDItemCFBundleIdentifier = \"(null)\"
kMDItemCFBundleIdentifier = \"com.apple.Safari\"
kMDItemCFBundleIdentifier = \"com.googlecode.iterm2\"
";
    let bids = parse_bundle_identifiers(mdls);
    assert_eq!(bids, vec!["com.apple.Safari", "com.googlecode.iterm2"]);
}
