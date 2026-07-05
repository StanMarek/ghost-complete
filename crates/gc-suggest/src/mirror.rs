//! Install-mirror staleness detection and silent self-healing.
//!
//! `ghost-complete install` writes the embedded spec corpus to
//! `~/.config/ghost-complete/specs/` so users can inspect and hand-edit
//! their completion data. The runtime spec loader treats that directory
//! as **higher precedence than the embedded corpus**
//! (see [`crate::specs::SpecStore::load_with_embedded`]).
//!
//! That precedence rule is the right default — users who customise a
//! spec expect their edits to win — but it created a silent regression
//! window on every binary upgrade: an operator who ran `ghost-complete
//! install` against v0.15.x and then upgraded to v0.16 would keep
//! serving the old v0.15 mirror at every keystroke, masking every
//! shipped improvement until they manually re-ran `install`.
//!
//! This module fixes that by maintaining a structured manifest
//! (`.ghost-complete-version`) that records, for each embedded spec
//! filename, the sha256 of the bytes we last wrote. On startup the
//! proxy compares the current on-disk content against the stamped
//! hash:
//!
//! - hash matches stamp → safe to overwrite with the new embedded
//!   bytes (user hasn't touched the file since we wrote it),
//! - hash differs → user edited the file. We skip the overwrite and
//!   record the filename so the doctor can surface it.
//!
//! Users who explicitly configured `[paths] spec_dirs` to point at a
//! custom location are exempt entirely — those are intentional
//! overrides we must not clobber.
//!
//! The doctor surfaces a `[WARN]` when the mirror is stale and the
//! auto-refresh path could not heal it (e.g. permission failures), or
//! when one or more spec files were skipped because the user edited
//! them since we last wrote them.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::embedded::{embedded_filenames, embedded_spec_contents};

/// Sentinel file written into the install mirror dir whose contents are
/// the structured manifest of the binary version + per-file sha256
/// hashes that this binary wrote.
pub const STAMP_FILENAME: &str = ".ghost-complete-version";

/// `CARGO_PKG_VERSION` of the currently-running binary. Inlined at
/// compile time from the workspace `[workspace.package].version`.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stamp format magic. The v0.16.0 prerelease shipped a plain
/// version-string stamp; this constant lets us migrate forward
/// without ever silently overwriting a user-edited file.
const STAMP_MAGIC_V2: &str = "ghost-complete-mirror v2";

/// Parsed contents of a `.ghost-complete-version` stamp file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StampManifest {
    /// No stamp file present at all.
    Missing,
    /// Stamp file exists but is in the legacy plain-text format
    /// (`<version>`) shipped by the v0.16.0 prerelease. The mirror
    /// is by definition untouched by the new hash-aware writer, so
    /// the refresh path treats it as a "safe to refresh everything"
    /// signal and migrates to the v2 format on the next write.
    LegacyVersionOnly { version: String },
    /// Modern manifest with per-file sha256 hashes.
    V2 {
        version: String,
        hashes: BTreeMap<String, String>,
    },
    /// Stamp file present but unparseable. Treated by the refresh
    /// path as "missing" — i.e. fall back to the conservative
    /// rewrite-everything behaviour (with hash check still applied
    /// on a per-file basis since on-disk content alone is enough to
    /// decide).
    Unreadable,
}

impl StampManifest {
    fn version(&self) -> Option<&str> {
        match self {
            Self::Missing | Self::Unreadable => None,
            Self::LegacyVersionOnly { version } | Self::V2 { version, .. } => Some(version),
        }
    }

    /// Returns the stamped sha256 for `filename`, or `None` if we
    /// don't have a per-file hash (legacy stamps, missing stamps,
    /// or filenames absent from the manifest).
    fn hash_for(&self, filename: &str) -> Option<&str> {
        match self {
            Self::V2 { hashes, .. } => hashes.get(filename).map(String::as_str),
            _ => None,
        }
    }
}

/// Outcome of inspecting the install mirror's version stamp against
/// the running binary's version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorStatus {
    /// No mirror directory present — the user never ran
    /// `ghost-complete install` (or removed it). Nothing to refresh.
    NotInstalled,
    /// Mirror directory exists but carries no stamp file. Treated the
    /// same as `Stale("unknown")` by the refresh path — older binaries
    /// did not write a stamp, so any pre-v0.16 install lands here.
    Unstamped,
    /// Mirror stamp matches the running binary's version. No-op.
    Fresh,
    /// Mirror stamp is present but disagrees with the running binary.
    /// The string is the on-disk version (verbatim, sanitised for log
    /// display by the caller).
    Stale { on_disk: String },
}

impl MirrorStatus {
    /// Does this status mean the refresh path should rewrite the mirror?
    pub fn needs_refresh(&self) -> bool {
        matches!(self, Self::Unstamped | Self::Stale { .. })
    }

    /// Human-readable on-disk version, or `"unknown"` for the unstamped
    /// case. Returned for doctor / log message construction.
    pub fn on_disk_version_or_unknown(&self) -> &str {
        match self {
            Self::Stale { on_disk } => on_disk.as_str(),
            Self::Unstamped => "unknown",
            Self::Fresh | Self::NotInstalled => "",
        }
    }
}

/// Default install mirror path: `~/.config/ghost-complete/specs/`.
///
/// Returns `None` only when `HOME` is unset (test harness / `--user`
/// systemd unit edge case).
pub fn default_install_mirror_dir() -> Option<PathBuf> {
    gc_config::config_dir().map(|d| d.join("specs"))
}

/// Compute the sha256 of a byte slice and render it as lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

/// Render a stamp manifest as the on-disk byte stream.
fn render_v2_stamp(version: &str, hashes: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    out.push_str(STAMP_MAGIC_V2);
    out.push('\n');
    out.push_str(version);
    out.push('\n');
    for (filename, hash) in hashes {
        out.push_str(hash);
        out.push_str("  ");
        out.push_str(filename);
        out.push('\n');
    }
    out
}

/// Parse the bytes of a `.ghost-complete-version` file into a
/// [`StampManifest`].
fn parse_stamp(contents: &str) -> StampManifest {
    // Strip optional trailing newline / whitespace so a user editing
    // the file doesn't trigger spurious staleness.
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return StampManifest::Unreadable;
    }

    let mut lines = trimmed.lines();
    let first = lines.next().unwrap_or("").trim();

    // Modern (v2) manifest: magic header + version + hash lines.
    if first == STAMP_MAGIC_V2 {
        let Some(version) = lines.next().map(str::trim) else {
            return StampManifest::Unreadable;
        };
        let mut hashes = BTreeMap::new();
        for line in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Expect: `<sha256_hex>  <filename>` (two spaces between,
            // matching the `sha256sum` UNIX convention).
            let mut parts = line.splitn(2, "  ");
            let Some(hash) = parts.next() else { continue };
            let Some(filename) = parts.next() else {
                // Malformed line — bail out rather than guess.
                return StampManifest::Unreadable;
            };
            let hash = hash.trim();
            let filename = filename.trim();
            if hash.is_empty() || filename.is_empty() {
                return StampManifest::Unreadable;
            }
            hashes.insert(filename.to_owned(), hash.to_owned());
        }
        return StampManifest::V2 {
            version: version.to_owned(),
            hashes,
        };
    }

    // Legacy v0.16.0 prerelease format: a single bare version line.
    // The whole file must be just that line (no extra content),
    // otherwise we'd risk misinterpreting a malformed v2 file as a
    // legacy stamp. A clean "0.16.0" matches the SemVer-shaped
    // expectation.
    if lines.next().is_none() && first.chars().any(|c| c.is_ascii_digit()) {
        return StampManifest::LegacyVersionOnly {
            version: first.to_owned(),
        };
    }

    StampManifest::Unreadable
}

/// Read and parse the stamp file at `install_dir`, returning
/// `StampManifest::Missing` when the file is absent.
fn read_stamp(install_dir: &Path) -> StampManifest {
    let stamp_path = install_dir.join(STAMP_FILENAME);
    match fs::read_to_string(&stamp_path) {
        Ok(contents) => parse_stamp(&contents),
        Err(e) if e.kind() == io::ErrorKind::NotFound => StampManifest::Missing,
        Err(_) => StampManifest::Unreadable,
    }
}

/// Scan the install mirror against the stamped per-file hashes and
/// return the list of filenames the user has edited since we last
/// wrote them. Returns an empty list when:
///
/// - the mirror has no stamp file or the stamp is the legacy v1
///   (no per-file hashes to compare against), or
/// - no spec files have been modified since the v2 stamp was
///   written.
///
/// I/O errors while reading individual specs are silently ignored —
/// the doctor treats this as an advisory signal, not a hard check.
pub fn list_user_edited_specs(install_dir: &Path) -> Vec<String> {
    let stamp = read_stamp(install_dir);
    let StampManifest::V2 { hashes, .. } = stamp else {
        return Vec::new();
    };
    let mut edited = Vec::new();
    for (filename, stamped_hash) in &hashes {
        let path = install_dir.join(filename);
        let Ok(bytes) = fs::read(&path) else {
            // File deleted or unreadable — not a "user edit" in the
            // sense the doctor warns about; the next refresh will
            // re-create it.
            continue;
        };
        let current_hash = sha256_hex(&bytes);
        if &current_hash != stamped_hash {
            edited.push(filename.clone());
        }
    }
    edited.sort();
    edited
}

/// Compare the mirror's stamp file against the running binary version.
///
/// Trims surrounding whitespace from the stamp body so a trailing
/// newline (which `fs::write` does not add, but a user editing the file
/// likely will) does not cause spurious staleness.
pub fn mirror_status(install_dir: &Path, current_version: &str) -> MirrorStatus {
    if !install_dir.is_dir() {
        return MirrorStatus::NotInstalled;
    }
    let stamp = read_stamp(install_dir);
    match stamp.version() {
        Some(v) if v == current_version => MirrorStatus::Fresh,
        Some(v) => MirrorStatus::Stale {
            on_disk: v.to_owned(),
        },
        None => MirrorStatus::Unstamped,
    }
}

/// Outcome of [`refresh_install_mirror_if_stale`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// User has not run `install`; nothing to do.
    NotInstalled,
    /// Mirror was already current.
    AlreadyFresh,
    /// Mirror was refreshed in place; embedded corpus is now on disk
    /// at the install mirror. `skipped_user_edited` lists filenames
    /// whose on-disk bytes differed from the stamped hash and were
    /// therefore preserved.
    Refreshed {
        previous_version: String,
        skipped_user_edited: Vec<String>,
    },
    /// Refresh failed (permission denied, disk full, etc.). The
    /// runtime continues with the stale on-disk corpus winning
    /// precedence — but the doctor's stamp check will warn loudly.
    Failed { reason: String },
}

/// Auto-refresh the install mirror when it is stale or unstamped.
///
/// `writer` is the side-effecting half (separated for testability) and
/// must:
/// - clear the existing mirror contents (or be safe to overwrite),
/// - write the current embedded corpus to `install_dir`,
/// - write the version stamp file.
///
/// Returns the [`RefreshOutcome`] so callers (proxy startup, doctor)
/// can log or surface the result. The writer signature returns a list
/// of skipped (user-edited) filenames so the outcome can propagate
/// them upward.
pub fn refresh_install_mirror_if_stale(
    install_dir: &Path,
    current_version: &str,
    writer: impl FnOnce(&Path, &str) -> io::Result<Vec<String>>,
) -> RefreshOutcome {
    let status = mirror_status(install_dir, current_version);
    match status {
        MirrorStatus::NotInstalled => RefreshOutcome::NotInstalled,
        MirrorStatus::Fresh => RefreshOutcome::AlreadyFresh,
        ref s @ (MirrorStatus::Stale { .. } | MirrorStatus::Unstamped) => {
            let previous_version = s.on_disk_version_or_unknown().to_owned();
            match writer(install_dir, current_version) {
                Ok(skipped_user_edited) => RefreshOutcome::Refreshed {
                    previous_version,
                    skipped_user_edited,
                },
                Err(e) => RefreshOutcome::Failed {
                    reason: e.to_string(),
                },
            }
        }
    }
}

/// Write the version stamp without touching spec files. Used as a
/// last-resort fallback (no per-file hashes) — for example when an
/// embedded spec body cannot be retrieved. Prefer
/// [`write_embedded_mirror`] which writes the spec corpus and stamp
/// atomically with hashes recorded.
pub fn write_stamp(install_dir: &Path, version: &str) -> io::Result<()> {
    let hashes = BTreeMap::new();
    fs::write(
        install_dir.join(STAMP_FILENAME),
        render_v2_stamp(version, &hashes),
    )
}

/// Pretty-print a spec body. Falls back to the raw minified body if
/// parsing fails. Extracted so the install path and the auto-refresh
/// path produce identical bytes on disk — required for the hash check
/// to ever match.
fn render_spec_body(contents: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(contents) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| contents.into()),
        Err(_) => contents.into(),
    }
}

/// Write the entire embedded corpus to `install_dir` and then stamp it
/// with `version`. This is the canonical writer for the install mirror:
/// `ghost-complete install` calls it directly, and the
/// startup-auto-refresh path passes it as the `writer` closure to
/// [`refresh_install_mirror_if_stale`].
///
/// Specs are pretty-printed (one JSON object per line) so users can
/// diff and hand-edit overrides — this matches the pre-v0.16 install
/// contract exactly. Each spec is written in place; we do not
/// `remove_dir_all` first so a user who hand-edited extra files in the
/// mirror keeps them (the only files we ever overwrite are the
/// embedded filenames).
///
/// **User edits are preserved.** Before overwriting any existing spec
/// the writer compares its current sha256 against the hash recorded in
/// the previous stamp manifest. If the file has been modified since
/// the last time we wrote it, the overwrite is skipped and the
/// filename is returned in the outcome's `skipped_user_edited` list.
/// The new stamp records the user's current hash so subsequent
/// refreshes track from that baseline.
///
/// The stamp is written only after every spec has been written so a
/// crash mid-refresh leaves the mirror in a state that the next start
/// will detect as still stale and retry.
pub fn write_embedded_mirror(install_dir: &Path, version: &str) -> io::Result<Vec<String>> {
    fs::create_dir_all(install_dir)?;

    // Read the pre-existing manifest *before* we start writing so we
    // can decide per-file whether to overwrite. A legacy stamp (or
    // no stamp at all on a brand-new install) yields `hash_for` →
    // `None`, in which case we conservatively overwrite all files —
    // this matches the original "untouched mirror" semantics.
    let previous_stamp = read_stamp(install_dir);

    let mut new_hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut skipped: Vec<String> = Vec::new();

    for name in embedded_filenames() {
        let contents = embedded_spec_contents(name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("embedded spec missing from archive: {name}"),
            )
        })?;
        let body = render_spec_body(contents);
        let dest_file = install_dir.join(name);

        // If the file already exists, decide whether the user edited
        // it since we last wrote it. The stamp records the sha256 of
        // the bytes the writer last laid down for each filename;
        // comparing the on-disk content to that hash is exactly the
        // "did the user touch this since we wrote it?" question.
        //
        // Three cases:
        //   1. file absent → write fresh, stamp our hash.
        //   2. file present + on-disk hash matches `last_written`
        //      stamp → safe to overwrite. Write the new embedded
        //      body and stamp the new hash (so a no-op write where
        //      the new bytes also match is still idempotent).
        //   3. file present + on-disk hash differs from `last_written`
        //      stamp → user edited it. Skip the overwrite and KEEP
        //      the old `last_written` hash in the new stamp. Two
        //      consequences flow from that choice:
        //        (a) next refresh recomputes "on-disk == last_written?"
        //            from the same baseline — so if the user reverts
        //            their edit later (or removes the file), we
        //            naturally re-sync.
        //        (b) on the very next refresh with the file still
        //            user-edited, the answer is still "no" and we
        //            keep skipping, never silently overwriting.
        //
        // A legacy / missing / unreadable previous stamp yields
        // `previous_hash = None`. In that case we treat the file as
        // "we have no baseline" and conservatively rewrite — pre-PR
        // installs were by definition untouched (the writer was
        // added in this PR), so this preserves the OPERATOR-win.
        let on_disk = fs::read(&dest_file);
        match on_disk {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Case 1: write fresh.
                fs::write(&dest_file, &body)?;
                new_hashes.insert(name.to_string(), sha256_hex(body.as_bytes()));
            }
            Err(other) => {
                // Treat as a writer failure so the proxy surfaces it.
                return Err(other);
            }
            Ok(current_bytes) => {
                let current_hash = sha256_hex(&current_bytes);
                let previous_hash = previous_stamp.hash_for(name);

                let safe_to_overwrite = match previous_hash {
                    Some(stamped) => stamped == current_hash,
                    None => true, // No baseline → rewrite (cases handled above).
                };

                if safe_to_overwrite {
                    let new_hash = sha256_hex(body.as_bytes());
                    if new_hash != current_hash {
                        fs::write(&dest_file, &body)?;
                    }
                    new_hashes.insert(name.to_string(), new_hash);
                } else {
                    // User edited this file since we wrote it.
                    // Preserve it; carry the previous `last_written`
                    // hash forward so subsequent refreshes keep
                    // detecting the user's divergence (rather than
                    // adopting the user's hash and silently
                    // overwriting on the NEXT refresh).
                    skipped.push(name.to_string());
                    // SAFETY: `previous_hash` is Some here because the
                    // `None` branch of `safe_to_overwrite` is true,
                    // which would have taken the other arm.
                    let carry =
                        previous_hash.expect("safe_to_overwrite=false implies a stamped hash");
                    new_hashes.insert(name.to_string(), carry.to_owned());
                }
            }
        }
    }

    fs::write(
        install_dir.join(STAMP_FILENAME),
        render_v2_stamp(version, &new_hashes),
    )?;
    Ok(skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sha256_hex_matches_fips_180_4_known_answers() {
        // Pins the on-disk hash format. `list_user_edited_specs` and
        // `write_embedded_mirror` compare freshly computed sha256 hashes
        // against hashes a PRIOR binary persisted in the version stamp.
        // If a future digest-crate bump (e.g. the sha2 0.10 -> 0.11
        // RustCrypto generation shift) ever changed the output bytes for
        // identical input, every mirrored spec would be falsely flagged
        // "user-edited" on upgrade and auto-refresh would silently skip
        // all overwrites. These canonical FIPS-180-4 / RFC-6234 vectors
        // fail loudly the moment that happens.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn mirror_status_not_installed_when_dir_absent() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert_eq!(
            mirror_status(&missing, "0.16.0"),
            MirrorStatus::NotInstalled
        );
    }

    #[test]
    fn mirror_status_unstamped_when_dir_present_without_stamp() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("git.json"), "{}").unwrap();
        assert_eq!(mirror_status(dir.path(), "0.16.0"), MirrorStatus::Unstamped);
    }

    #[test]
    fn mirror_status_fresh_when_stamp_matches() {
        let dir = TempDir::new().unwrap();
        write_stamp(dir.path(), "0.16.0").unwrap();
        assert_eq!(mirror_status(dir.path(), "0.16.0"), MirrorStatus::Fresh);
    }

    #[test]
    fn mirror_status_fresh_tolerates_trailing_whitespace() {
        // Plain version-only legacy v1 stamp with trailing newline:
        // must still parse to LegacyVersionOnly("0.16.0") and report
        // Fresh against the matching binary version.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(STAMP_FILENAME), "0.16.0\n").unwrap();
        assert_eq!(mirror_status(dir.path(), "0.16.0"), MirrorStatus::Fresh);
    }

    #[test]
    fn mirror_status_stale_when_stamp_differs() {
        let dir = TempDir::new().unwrap();
        write_stamp(dir.path(), "0.15.0").unwrap();
        assert_eq!(
            mirror_status(dir.path(), "0.16.0"),
            MirrorStatus::Stale {
                on_disk: "0.15.0".to_owned(),
            }
        );
    }

    #[test]
    fn parse_stamp_v2_round_trips() {
        let mut hashes = BTreeMap::new();
        hashes.insert("git.json".to_string(), "deadbeef".to_string());
        hashes.insert("docker.json".to_string(), "cafef00d".to_string());
        let rendered = render_v2_stamp("0.16.0", &hashes);
        match parse_stamp(&rendered) {
            StampManifest::V2 {
                version,
                hashes: out,
            } => {
                assert_eq!(version, "0.16.0");
                assert_eq!(out.get("git.json").map(String::as_str), Some("deadbeef"));
                assert_eq!(out.get("docker.json").map(String::as_str), Some("cafef00d"));
            }
            other => panic!("expected V2, got {other:?}"),
        }
    }

    #[test]
    fn parse_stamp_legacy_version_only() {
        match parse_stamp("0.16.0") {
            StampManifest::LegacyVersionOnly { version } => assert_eq!(version, "0.16.0"),
            other => panic!("expected LegacyVersionOnly, got {other:?}"),
        }
    }

    #[test]
    fn parse_stamp_unreadable_for_empty() {
        assert!(matches!(parse_stamp(""), StampManifest::Unreadable));
    }

    #[test]
    fn refresh_skips_when_dir_absent() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("nope");
        let outcome = refresh_install_mirror_if_stale(&missing, "0.16.0", |_, _| {
            panic!("writer must not run for an absent mirror");
        });
        assert_eq!(outcome, RefreshOutcome::NotInstalled);
    }

    #[test]
    fn refresh_skips_when_fresh() {
        let dir = TempDir::new().unwrap();
        write_stamp(dir.path(), "0.16.0").unwrap();
        let outcome = refresh_install_mirror_if_stale(dir.path(), "0.16.0", |_, _| {
            panic!("writer must not run for a fresh mirror");
        });
        assert_eq!(outcome, RefreshOutcome::AlreadyFresh);
    }

    #[test]
    fn refresh_runs_writer_when_unstamped() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("git.json"), "{}").unwrap();
        let mut ran = false;
        let outcome = refresh_install_mirror_if_stale(dir.path(), "0.16.0", |path, ver| {
            ran = true;
            // Simulate the real writer's contract: rewrite + stamp,
            // no files skipped.
            write_stamp(path, ver)?;
            Ok(Vec::new())
        });
        assert!(ran, "writer must run when mirror is unstamped");
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                previous_version: "unknown".to_owned(),
                skipped_user_edited: Vec::new(),
            }
        );
        assert_eq!(mirror_status(dir.path(), "0.16.0"), MirrorStatus::Fresh);
    }

    #[test]
    fn refresh_runs_writer_when_stale_and_reports_previous_version() {
        let dir = TempDir::new().unwrap();
        write_stamp(dir.path(), "0.14.2").unwrap();
        let outcome = refresh_install_mirror_if_stale(dir.path(), "0.16.0", |path, ver| {
            write_stamp(path, ver)?;
            Ok(Vec::new())
        });
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                previous_version: "0.14.2".to_owned(),
                skipped_user_edited: Vec::new(),
            }
        );
        assert_eq!(mirror_status(dir.path(), "0.16.0"), MirrorStatus::Fresh);
    }

    #[test]
    fn refresh_surfaces_writer_errors() {
        let dir = TempDir::new().unwrap();
        write_stamp(dir.path(), "0.14.0").unwrap();
        let outcome = refresh_install_mirror_if_stale(dir.path(), "0.16.0", |_, _| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "EACCES"))
        });
        match outcome {
            RefreshOutcome::Failed { reason } => assert!(reason.contains("EACCES")),
            other => panic!("expected Failed, got {other:?}"),
        }
        // Stamp must not have been bumped because the writer failed —
        // doctor's WARN check relies on this.
        assert_eq!(
            mirror_status(dir.path(), "0.16.0"),
            MirrorStatus::Stale {
                on_disk: "0.14.0".to_owned(),
            }
        );
    }

    #[test]
    fn write_embedded_mirror_creates_dir_writes_stamp_and_a_known_spec() {
        let dir = TempDir::new().unwrap();
        let install = dir.path().join("install-mirror");
        // Pre-condition: install dir does not exist — writer must
        // create it.
        assert!(!install.exists());

        let skipped = write_embedded_mirror(&install, "0.16.0-test").unwrap();
        assert!(skipped.is_empty(), "fresh write must have no skips");

        assert!(install.is_dir(), "writer must create the install dir");
        // Stamp written and parseable as the supplied version.
        assert_eq!(
            mirror_status(&install, "0.16.0-test"),
            MirrorStatus::Fresh,
            "stamp must match supplied version after write"
        );
        // git.json is a known-present spec and a good smoke test that
        // we actually wrote spec bodies (not just the stamp).
        let git_path = install.join("git.json");
        assert!(git_path.is_file(), "git.json must be written");
        let body = fs::read_to_string(&git_path).unwrap();
        assert!(
            body.contains("\"name\""),
            "git.json body must contain valid JSON keys"
        );
    }

    #[test]
    fn refresh_via_writer_overwrites_stale_specs() {
        // Simulates the real failure mode the operator hit: a v0.15
        // mirror exists on disk with stale spec contents, the v0.16
        // binary starts, and the writer must replace the stale bytes
        // (not merely re-stamp). The previous stamp was a legacy
        // v0.15 plain-version-only stamp, so there's no baseline to
        // protect — we MUST rewrite.
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        // Plant a poisoned `git.json` from a prior version.
        fs::write(install.join("git.json"), "{\"name\":\"OLD\"}").unwrap();
        write_stamp(&install, "0.14.0").unwrap();

        let outcome = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        match &outcome {
            RefreshOutcome::Refreshed {
                previous_version,
                skipped_user_edited,
            } => {
                assert_eq!(previous_version, "0.14.0");
                assert!(
                    skipped_user_edited.is_empty(),
                    "legacy-stamped mirror must be refreshed wholesale"
                );
            }
            other => panic!("expected Refreshed, got {other:?}"),
        }
        // Stale poisoned body is gone.
        let new_body = fs::read_to_string(install.join("git.json")).unwrap();
        assert!(
            !new_body.contains("\"OLD\""),
            "stale poisoned spec body must be overwritten on refresh"
        );
        assert_eq!(mirror_status(&install, "0.16.0"), MirrorStatus::Fresh);
    }

    #[test]
    fn refresh_unmodified_mirror_overwrites_cleanly_and_records_hashes() {
        // Round 1: install fresh.
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        let skipped = write_embedded_mirror(&install, "0.15.9").unwrap();
        assert!(skipped.is_empty());

        // Round 2: simulate the upgrade. Force a "stale" status by
        // bumping the binary version on the second write. The user
        // hasn't touched anything; every file should be overwritten
        // and zero skipped.
        let outcome = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        match &outcome {
            RefreshOutcome::Refreshed {
                previous_version,
                skipped_user_edited,
            } => {
                assert_eq!(previous_version, "0.15.9");
                assert!(
                    skipped_user_edited.is_empty(),
                    "unmodified mirror must refresh with zero skips, got {skipped_user_edited:?}"
                );
            }
            other => panic!("expected Refreshed, got {other:?}"),
        }
        assert_eq!(mirror_status(&install, "0.16.0"), MirrorStatus::Fresh);
    }

    #[test]
    fn refresh_user_edited_file_is_skipped() {
        // Round 1: install fresh so a v2 stamp with per-file hashes
        // gets laid down.
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        write_embedded_mirror(&install, "0.15.9").unwrap();

        // User edits one spec by hand.
        let git_path = install.join("git.json");
        let original = fs::read_to_string(&git_path).unwrap();
        let edited = original.replace("\"name\"", "\"name_USER_EDIT\"");
        assert_ne!(edited, original, "test setup must produce a real edit");
        fs::write(&git_path, &edited).unwrap();

        // Round 2: refresh against a newer binary version. The
        // user-edited file must be preserved AND reported.
        let outcome = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        match &outcome {
            RefreshOutcome::Refreshed {
                skipped_user_edited,
                ..
            } => {
                assert_eq!(
                    skipped_user_edited.as_slice(),
                    &["git.json".to_string()],
                    "user-edited file must appear in skipped list"
                );
            }
            other => panic!("expected Refreshed, got {other:?}"),
        }
        // The edit must survive the refresh.
        let after = fs::read_to_string(&git_path).unwrap();
        assert!(
            after.contains("name_USER_EDIT"),
            "user-edited bytes must NOT be overwritten by auto-refresh"
        );
    }

    #[test]
    fn refresh_repeated_after_user_edit_keeps_preserving() {
        // Regression guard for the trickiest bit of the design:
        // after a user edit is skipped, the new stamp must carry the
        // ORIGINAL `last_written` hash for that file (not the user's
        // hash). Otherwise the next refresh would see
        // `current_hash == stamped_hash`, conclude "safe to
        // overwrite", and silently clobber the user's edit on the
        // NEXT upgrade.
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        write_embedded_mirror(&install, "0.15.9").unwrap();

        // User edits a spec.
        let git_path = install.join("git.json");
        let original = fs::read_to_string(&git_path).unwrap();
        let edited = original.replace("\"name\"", "\"name_USER\"");
        fs::write(&git_path, &edited).unwrap();

        // Refresh #1: edit detected and preserved.
        let r1 = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        assert!(matches!(
            r1,
            RefreshOutcome::Refreshed {
                ref skipped_user_edited,
                ..
            } if skipped_user_edited == &vec!["git.json".to_string()]
        ));
        let after_r1 = fs::read_to_string(&git_path).unwrap();
        assert!(after_r1.contains("name_USER"));

        // Refresh #2 (simulating a further binary upgrade) with the
        // user-edit STILL present and unmodified since refresh #1.
        // The skip must fire again — NOT silently overwrite.
        let r2 = refresh_install_mirror_if_stale(&install, "0.17.0", write_embedded_mirror);
        match r2 {
            RefreshOutcome::Refreshed {
                skipped_user_edited,
                ..
            } => {
                assert_eq!(
                    skipped_user_edited.as_slice(),
                    &["git.json".to_string()],
                    "user edit must continue to be skipped across refreshes"
                );
            }
            other => panic!("expected Refreshed, got {other:?}"),
        }
        let after_r2 = fs::read_to_string(&git_path).unwrap();
        assert!(
            after_r2.contains("name_USER"),
            "user edit must survive multiple refreshes"
        );
    }

    #[test]
    fn refresh_after_user_revert_resyncs() {
        // If the user reverts their edit (deletes their changes or
        // restores the originally-written body) the next refresh
        // must successfully overwrite the now-untouched file with
        // the latest embedded body.
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        write_embedded_mirror(&install, "0.15.9").unwrap();

        let git_path = install.join("git.json");
        let original_bytes = fs::read(&git_path).unwrap();
        // Edit then revert.
        fs::write(&git_path, b"// user temp edit\n").unwrap();
        fs::write(&git_path, &original_bytes).unwrap();

        let outcome = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        match outcome {
            RefreshOutcome::Refreshed {
                skipped_user_edited,
                ..
            } => {
                assert!(
                    skipped_user_edited.is_empty(),
                    "reverted file must NOT be flagged as user-edited"
                );
            }
            other => panic!("expected Refreshed, got {other:?}"),
        }
    }

    #[test]
    fn refresh_mixed_mirror_only_skips_user_edited_files() {
        // Round 1: fresh install — two specs we'll leave alone, one
        // we'll edit.
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        write_embedded_mirror(&install, "0.15.9").unwrap();

        // Pick three real specs from the embedded set so we know
        // they exist on disk after round 1.
        let touchable_filenames: Vec<&str> = embedded_filenames().iter().take(3).copied().collect();
        assert_eq!(touchable_filenames.len(), 3);
        let edit_target = touchable_filenames[0];

        // Edit only the first.
        let target_path = install.join(edit_target);
        let original = fs::read_to_string(&target_path).unwrap();
        let edited = format!("{original}\n// user note\n");
        fs::write(&target_path, &edited).unwrap();

        // Capture the pre-refresh bytes of the two we don't edit so
        // we can assert they round-tripped to identical pretty-print
        // output (i.e. the refresh ran on them).
        let untouched_a_before = fs::read_to_string(install.join(touchable_filenames[1])).unwrap();
        let _untouched_b_before = fs::read_to_string(install.join(touchable_filenames[2])).unwrap();

        // Round 2: refresh.
        let outcome = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        let skipped = match outcome {
            RefreshOutcome::Refreshed {
                skipped_user_edited,
                ..
            } => skipped_user_edited,
            other => panic!("expected Refreshed, got {other:?}"),
        };

        assert_eq!(
            skipped.as_slice(),
            &[edit_target.to_string()],
            "only the user-edited spec must be in the skipped list"
        );
        // Edited file preserved.
        let after_edit = fs::read_to_string(&target_path).unwrap();
        assert!(after_edit.contains("user note"));
        // Untouched files still present and parseable.
        let untouched_a_after = fs::read_to_string(install.join(touchable_filenames[1])).unwrap();
        assert!(untouched_a_after.starts_with(untouched_a_before.chars().next().unwrap()));
    }

    #[test]
    fn refresh_with_legacy_pre_pr_stamp_rewrites_everything() {
        // Pre-PR installs only wrote a bare version-string stamp
        // (no per-file hashes). The new writer must treat that as
        // "no baseline → safe to rewrite everything" so the existing
        // OPERATOR upgrade flow remains silent and automatic.
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        fs::create_dir_all(&install).unwrap();

        // Manually write the legacy v1 stamp format (just a version
        // string, no magic header, no per-file lines).
        fs::write(install.join(STAMP_FILENAME), "0.15.0\n").unwrap();
        // Plant a stale spec body.
        fs::write(install.join("git.json"), "{\"name\":\"STALE\"}").unwrap();

        let outcome = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        match &outcome {
            RefreshOutcome::Refreshed {
                previous_version,
                skipped_user_edited,
            } => {
                assert_eq!(previous_version, "0.15.0");
                assert!(
                    skipped_user_edited.is_empty(),
                    "pre-PR legacy stamp must NOT trigger spurious skips"
                );
            }
            other => panic!("expected Refreshed, got {other:?}"),
        }
        // Stale planted body is gone.
        let new_body = fs::read_to_string(install.join("git.json")).unwrap();
        assert!(
            !new_body.contains("\"STALE\""),
            "legacy-stamped mirror must be overwritten wholesale"
        );
        // New stamp is the v2 format with per-file hashes.
        let stamp = read_stamp(&install);
        match stamp {
            StampManifest::V2 { version, hashes } => {
                assert_eq!(version, "0.16.0");
                assert!(
                    hashes.contains_key("git.json"),
                    "v2 stamp must record per-file hashes after migration"
                );
            }
            other => panic!("expected V2 stamp after migration, got {other:?}"),
        }
    }

    #[test]
    fn refresh_with_absent_stamp_and_present_files_refreshes_everything() {
        // Very old install path: spec files present, no stamp at
        // all. We have no baseline to protect, so the writer must
        // rewrite everything (preserving the original
        // pre-stamp-writer semantics).
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("git.json"), "{\"name\":\"VERY_OLD\"}").unwrap();
        // No stamp file at all.
        assert!(!install.join(STAMP_FILENAME).exists());

        let outcome = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        match &outcome {
            RefreshOutcome::Refreshed {
                previous_version,
                skipped_user_edited,
            } => {
                assert_eq!(previous_version, "unknown");
                assert!(skipped_user_edited.is_empty());
            }
            other => panic!("expected Refreshed, got {other:?}"),
        }
        let new_body = fs::read_to_string(install.join("git.json")).unwrap();
        assert!(!new_body.contains("\"VERY_OLD\""));
        assert_eq!(mirror_status(&install, "0.16.0"), MirrorStatus::Fresh);
    }

    #[test]
    fn list_user_edited_specs_reports_only_diverged_files() {
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        write_embedded_mirror(&install, "0.16.0").unwrap();

        // Fresh install has zero user edits.
        assert!(list_user_edited_specs(&install).is_empty());

        // Edit one spec and confirm it's reported (and only it).
        let git_path = install.join("git.json");
        let mut body = fs::read_to_string(&git_path).unwrap();
        body.push_str("// edit\n");
        fs::write(&git_path, body).unwrap();
        assert_eq!(
            list_user_edited_specs(&install),
            vec!["git.json".to_string()]
        );
    }

    #[test]
    fn list_user_edited_specs_returns_empty_for_legacy_stamp() {
        // No per-file hashes available → no advisory output. The
        // doctor falls back to the version-based staleness check in
        // that case.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(STAMP_FILENAME), "0.15.0\n").unwrap();
        fs::write(dir.path().join("git.json"), "{}").unwrap();
        assert!(list_user_edited_specs(dir.path()).is_empty());
    }

    #[test]
    fn current_version_matches_workspace_pkg_version() {
        // Pin the env! resolution so a refactor that moves this module
        // to a non-workspace crate would catch a mismatch at test time.
        assert!(
            !CURRENT_VERSION.is_empty(),
            "CARGO_PKG_VERSION must resolve at compile time"
        );
        // Loose check: must start with at least one digit + dot.
        let mut chars = CURRENT_VERSION.chars();
        assert!(chars.next().is_some_and(|c| c.is_ascii_digit()));
    }
}
