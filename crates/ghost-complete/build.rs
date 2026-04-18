//! Build script for ghost-complete.
//!
//! Emits two compile-time env vars consumed by clap's `--version` string:
//!
//! * `VERGEN_GIT_SHA` — short git SHA, or `"unknown"` if we cannot invoke
//!   `git` (e.g. cargo-dist source tarball).
//! * `VERGEN_BUILD_TIMESTAMP` — RFC3339 UTC build time.
//!
//! We deliberately avoid `vergen-gix` so this stays zero-dep and tolerant of
//! cargo-dist tarball builds that ship without a `.git` directory.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Re-run whenever HEAD moves (harmless miss when .git is absent).
    println!("cargo:rerun-if-changed=build.rs");
    // Only emit the directive when .git/HEAD actually exists. A missing path
    // is treated by Cargo as "always changed", which would re-run this script
    // on every build in cargo-dist tarballs / shallow clones where .git is
    // absent.
    if std::path::Path::new("../../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let sha = git_short_sha().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=VERGEN_GIT_SHA={sha}");

    let ts = build_timestamp();
    println!("cargo:rustc-env=VERGEN_BUILD_TIMESTAMP={ts}");
}

fn git_short_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=9", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Reproducible-build-friendly UTC timestamp.
/// Honors `SOURCE_DATE_EPOCH` when set (cargo-dist / reproducible builds).
fn build_timestamp() -> String {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
    format_rfc3339_utc(secs)
}

/// Format a unix timestamp as RFC3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`).
/// Zero-dep. Valid for 1970..9999.
fn format_rfc3339_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_days(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

// Howard Hinnant's days_from_civil inverse; integer-only.
fn civil_from_days(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let sod = (secs % 86_400) as u32;
    let h = sod / 3600;
    let mi = (sod % 3600) / 60;
    let s = sod % 60;

    // Shift epoch from 1970-01-01 to 0000-03-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = (y + if mo <= 2 { 1 } else { 0 }) as i32;

    (y, mo, d, h, mi, s)
}
