//! Integration tests for `macos_applications` + `macos_bundle_identifiers`.
//!
//! The pure parsers are pinned via the crate's public API so other
//! crates depending on `gc_suggest::providers::macos_apps::*` see the
//! same contract. The async `generate_with_binaries` fan-out is
//! `pub(crate)`, so we reach it by `include!`-ing the source module
//! (the same idiom `providers_kubectl.rs` uses) and driving it with
//! fake `mdfind`/`mdls` shell scripts.

use gc_suggest::providers::macos_apps::{
    app_paths_to_resolve, parse_applications, parse_bundle_identifiers,
};

// `crate::types` inside the included source module resolves against this
// crate root, so re-export the real types here.
pub mod types {
    pub use gc_suggest::types::*;
}

// `#[path = "."]` pins this module's directory to `tests/`, so the
// nested `#[path]` children below resolve relative to `tests/` (not the
// default `tests/providers/`). `#[path]` (not `include!`) keeps the
// included modules' leading `//!` inner doc comments valid: the file
// becomes the module body directly.
#[path = "."]
mod providers {
    pub use gc_suggest::providers::{Provider, ProviderCtx};

    #[path = "../src/providers/util.rs"]
    pub mod util;

    #[path = "../src/providers/macos_apps.rs"]
    pub mod macos_apps;
}

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use gc_suggest::providers::ProviderCtx;
use gc_suggest::types::{SuggestionKind, SuggestionSource};
use providers::macos_apps::{MacosApplications, MacosBundleIdentifiers};

fn ctx(cwd: &Path) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    }
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

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

#[test]
fn app_paths_to_resolve_filters_and_caps() {
    let mdfind = "\
/Applications/Safari.app
not-an-app
/Applications/Terminal.app
/System/Applications/Music.app
";
    // Cap of 2 keeps only the first two `.app` lines, dropping noise.
    let paths = app_paths_to_resolve(mdfind, 2);
    assert_eq!(
        paths,
        vec!["/Applications/Safari.app", "/Applications/Terminal.app"]
    );
}

/// End-to-end exercise of `MacosApplications::generate_with_binaries`
/// (the `open -a` provider): a fake `mdfind` prints two `.app` paths and
/// the live fan-out must map each to a suggestion whose `text` is the
/// display name (`Safari`) and whose `description` is the full bundle
/// path (`/Applications/Safari.app`). This pins the text != path mapping
/// — a text/description swap would otherwise pass every pure-parser test.
/// The `mdls` binary is unused by this provider, so it is never spawned.
#[cfg(unix)]
#[tokio::test]
async fn applications_maps_display_name_to_text_and_path_to_description() {
    let tmp = tempfile::TempDir::new().unwrap();

    let fake_mdfind = tmp.path().join("mdfind");
    // Ignore the query argument; print a canned application list.
    write_executable(
        &fake_mdfind,
        "#!/bin/sh\nprintf '%s\\n' \
'/Applications/Safari.app' \
'/Applications/Terminal.app'\n",
    );

    let suggestions = MacosApplications
        .generate_with_binaries(
            &ctx(tmp.path()),
            fake_mdfind.to_str().unwrap(),
            // mdls is unused by this provider; pass a sentinel that must
            // never be spawned.
            "/nonexistent/mdls-must-not-be-spawned",
        )
        .await
        .unwrap();

    assert_eq!(suggestions.len(), 2);
    assert_eq!(suggestions[0].text, "Safari");
    assert_eq!(
        suggestions[0].description.as_deref(),
        Some("/Applications/Safari.app"),
        "description must be the full bundle path, not the display name"
    );
    assert_eq!(suggestions[1].text, "Terminal");
    assert_eq!(
        suggestions[1].description.as_deref(),
        Some("/Applications/Terminal.app")
    );
    for suggestion in &suggestions {
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

/// End-to-end exercise of the semaphore-bounded `mdls` fan-out in
/// `MacosBundleIdentifiers::generate_with_binaries`: the fake `mdls`
/// emits a distinct bundle id per path and *sleeps* on the first path,
/// forcing `join_next()` to observe completions out of input order. The
/// reassembly-by-index glue must still return suggestions in input-path
/// order with each description equal to its source path.
#[cfg(unix)]
#[tokio::test]
async fn bundle_identifiers_reassembles_join_results_in_input_order() {
    let tmp = tempfile::TempDir::new().unwrap();

    let fake_mdfind = tmp.path().join("mdfind");
    // Three distinct `.app` paths, emitted in a fixed order. Ignore the
    // query argument; just print the canned application list.
    write_executable(
        &fake_mdfind,
        "#!/bin/sh\nprintf '%s\\n' \
'/Applications/Alpha.app' \
'/Applications/Beta.app' \
'/Applications/Gamma.app'\n",
    );

    let fake_mdls = tmp.path().join("mdls");
    // The app path is the final positional argument
    // (`mdls -name kMDItemCFBundleIdentifier -raw <path>`). Branch on it
    // to emit a distinct bundle id and force Alpha (the FIRST input
    // path) to finish LAST — proving the reassembly restores input order
    // regardless of completion order.
    //
    // Ordering is enforced with a sentinel-file barrier instead of a
    // fixed sleep that races the production MDLS_TIMEOUT_MS (1s): Beta
    // and Gamma `touch` a per-path `.done` marker after emitting their
    // id; Alpha spins (20ms granularity) until BOTH markers exist, then
    // emits. The spawned cwd is the tmp dir (`ctx(tmp.path())`), so the
    // markers land alongside the fake binaries. Alpha's task therefore
    // joins out of order yet finishes only ~tens of ms after Beta/Gamma
    // — never near the 1s timeout, even under CPU-starved parallel runs.
    // The poll is bounded (40 × 20ms = 0.8s worst case) so a missing
    // marker can never hang the lookup past the timeout.
    write_executable(
        &fake_mdls,
        "#!/bin/sh\n\
for arg in \"$@\"; do path=\"$arg\"; done\n\
case \"$path\" in\n\
  */Alpha.app)\n\
    i=0\n\
    while [ ! -f beta.done ] || [ ! -f gamma.done ]; do\n\
      i=$((i + 1))\n\
      [ \"$i\" -ge 40 ] && break\n\
      sleep 0.02\n\
    done\n\
    printf 'com.example.alpha\\n' ;;\n\
  */Beta.app) printf 'com.example.beta\\n'; : > beta.done ;;\n\
  */Gamma.app) printf 'com.example.gamma\\n'; : > gamma.done ;;\n\
  *) exit 1 ;;\n\
esac\n",
    );

    let suggestions = MacosBundleIdentifiers
        .generate_with_binaries(
            &ctx(tmp.path()),
            fake_mdfind.to_str().unwrap(),
            fake_mdls.to_str().unwrap(),
        )
        .await
        .unwrap();

    let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["com.example.alpha", "com.example.beta", "com.example.gamma"],
        "bundle ids must be in input-path order despite out-of-order mdls completion"
    );
    let descriptions: Vec<Option<&str>> = suggestions
        .iter()
        .map(|s| s.description.as_deref())
        .collect();
    assert_eq!(
        descriptions,
        vec![
            Some("/Applications/Alpha.app"),
            Some("/Applications/Beta.app"),
            Some("/Applications/Gamma.app"),
        ],
        "each description must be its own source path"
    );
}

/// When two distinct paths resolve to the SAME bundle id, cross-path
/// dedup must keep the first-input-path occurrence (text once, with the
/// first path as the description) — exercised through the live async
/// fan-out rather than the pure `assemble_bundle_identifiers` unit.
#[cfg(unix)]
#[tokio::test]
async fn bundle_identifiers_dedupes_same_id_across_paths_first_wins() {
    let tmp = tempfile::TempDir::new().unwrap();

    let fake_mdfind = tmp.path().join("mdfind");
    write_executable(
        &fake_mdfind,
        "#!/bin/sh\nprintf '%s\\n' \
'/Applications/Safari.app' \
'/Applications/Safari copy.app'\n",
    );

    let fake_mdls = tmp.path().join("mdls");
    // Both paths yield the identical bundle id; the FIRST path
    // (`Safari.app`) must finish LAST so the duplicate (second path,
    // `Safari copy.app`) lands first — dedup must still attribute the
    // surviving suggestion to the first input path. As above, force the
    // ordering with a sentinel-file barrier rather than a fixed sleep
    // that races the 1s production timeout: the second path touches
    // `dup.done` after emitting, and the first path spins (20ms, bounded
    // at 40 iters = 0.8s worst case) until that marker exists before
    // emitting. The cwd is the tmp dir, so the marker is visible to both
    // spawned processes. The non-`Safari.app` arm matches the second
    // path (`*` after the case for `Safari.app`).
    write_executable(
        &fake_mdls,
        "#!/bin/sh\n\
for arg in \"$@\"; do path=\"$arg\"; done\n\
case \"$path\" in\n\
  */Safari.app)\n\
    i=0\n\
    while [ ! -f dup.done ]; do\n\
      i=$((i + 1))\n\
      [ \"$i\" -ge 40 ] && break\n\
      sleep 0.02\n\
    done\n\
    printf 'com.apple.Safari\\n' ;;\n\
  *) printf 'com.apple.Safari\\n'; : > dup.done ;;\n\
esac\n",
    );

    let suggestions = MacosBundleIdentifiers
        .generate_with_binaries(
            &ctx(tmp.path()),
            fake_mdfind.to_str().unwrap(),
            fake_mdls.to_str().unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(suggestions.len(), 1, "duplicate bundle id must collapse");
    assert_eq!(suggestions[0].text, "com.apple.Safari");
    assert_eq!(
        suggestions[0].description.as_deref(),
        Some("/Applications/Safari.app"),
        "first input path must win the dedup and supply the description"
    );
}
