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
//! This module fixes that by stamping the mirror with the binary
//! version that wrote it (in `.ghost-complete-version`) and silently
//! refreshing the mirror on startup when the stamp is missing or older
//! than the current binary. Users who explicitly configured
//! `[paths] spec_dirs` to point at a custom location are exempt — those
//! are intentional overrides we must not clobber.
//!
//! The doctor surfaces a `[WARN]` when the mirror is stale and the
//! auto-refresh path could not heal it (e.g. permission failures).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::embedded::{embedded_filenames, embedded_spec_contents};

/// Sentinel file written into the install mirror dir whose contents are
/// the `CARGO_PKG_VERSION` of the binary that last refreshed it.
pub const STAMP_FILENAME: &str = ".ghost-complete-version";

/// `CARGO_PKG_VERSION` of the currently-running binary. Inlined at
/// compile time from the workspace `[workspace.package].version`.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// Compare the mirror's stamp file against the running binary version.
///
/// Trims surrounding whitespace from the stamp body so a trailing
/// newline (which `fs::write` does not add, but a user editing the file
/// likely will) does not cause spurious staleness.
pub fn mirror_status(install_dir: &Path, current_version: &str) -> MirrorStatus {
    if !install_dir.is_dir() {
        return MirrorStatus::NotInstalled;
    }
    let stamp_path = install_dir.join(STAMP_FILENAME);
    match fs::read_to_string(&stamp_path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed == current_version {
                MirrorStatus::Fresh
            } else {
                MirrorStatus::Stale {
                    on_disk: trimmed.to_owned(),
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => MirrorStatus::Unstamped,
        Err(_) => MirrorStatus::Unstamped,
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
    /// at the install mirror.
    Refreshed { previous_version: String },
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
/// can log or surface the result.
pub fn refresh_install_mirror_if_stale(
    install_dir: &Path,
    current_version: &str,
    writer: impl FnOnce(&Path, &str) -> io::Result<()>,
) -> RefreshOutcome {
    let status = mirror_status(install_dir, current_version);
    match status {
        MirrorStatus::NotInstalled => RefreshOutcome::NotInstalled,
        MirrorStatus::Fresh => RefreshOutcome::AlreadyFresh,
        ref s @ (MirrorStatus::Stale { .. } | MirrorStatus::Unstamped) => {
            let previous_version = s.on_disk_version_or_unknown().to_owned();
            match writer(install_dir, current_version) {
                Ok(()) => RefreshOutcome::Refreshed { previous_version },
                Err(e) => RefreshOutcome::Failed {
                    reason: e.to_string(),
                },
            }
        }
    }
}

/// Write the version stamp without touching spec files. Used by the
/// `install` command after `copy_specs` writes the corpus, and by the
/// proxy-startup refresh path after rewriting the mirror.
pub fn write_stamp(install_dir: &Path, version: &str) -> io::Result<()> {
    fs::write(install_dir.join(STAMP_FILENAME), version)
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
/// The stamp is written only after every spec has been written so a
/// crash mid-refresh leaves the mirror in a state that the next start
/// will detect as still stale and retry.
pub fn write_embedded_mirror(install_dir: &Path, version: &str) -> io::Result<()> {
    fs::create_dir_all(install_dir)?;
    for name in embedded_filenames() {
        let contents = embedded_spec_contents(name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("embedded spec missing from archive: {name}"),
            )
        })?;
        let dest_file = install_dir.join(name);
        // Pretty-print so on-disk specs match the historical install
        // layout (users diff them, hand-edit overrides). Fall back to
        // the raw minified body if a spec ever fails to parse — which
        // would only happen on a build-script bug, but better to land
        // a copy than to abort the whole refresh.
        let body = match serde_json::from_str::<serde_json::Value>(contents) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| contents.into()),
            Err(_) => contents.into(),
        };
        fs::write(&dest_file, body)?;
    }
    write_stamp(install_dir, version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
            // Simulate the real writer's contract: rewrite + stamp.
            write_stamp(path, ver)
        });
        assert!(ran, "writer must run when mirror is unstamped");
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                previous_version: "unknown".to_owned(),
            }
        );
        assert_eq!(mirror_status(dir.path(), "0.16.0"), MirrorStatus::Fresh);
    }

    #[test]
    fn refresh_runs_writer_when_stale_and_reports_previous_version() {
        let dir = TempDir::new().unwrap();
        write_stamp(dir.path(), "0.14.2").unwrap();
        let outcome = refresh_install_mirror_if_stale(dir.path(), "0.16.0", |path, ver| {
            write_stamp(path, ver)
        });
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                previous_version: "0.14.2".to_owned(),
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

        write_embedded_mirror(&install, "0.16.0-test").unwrap();

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
        // (not merely re-stamp).
        let dir = TempDir::new().unwrap();
        let install = dir.path().to_path_buf();
        // Plant a poisoned `git.json` from a prior version.
        fs::write(install.join("git.json"), "{\"name\":\"OLD\"}").unwrap();
        write_stamp(&install, "0.14.0").unwrap();

        let outcome = refresh_install_mirror_if_stale(&install, "0.16.0", write_embedded_mirror);
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                previous_version: "0.14.0".to_owned(),
            }
        );
        // Stale poisoned body is gone.
        let new_body = fs::read_to_string(install.join("git.json")).unwrap();
        assert!(
            !new_body.contains("\"OLD\""),
            "stale poisoned spec body must be overwritten on refresh"
        );
        assert_eq!(mirror_status(&install, "0.16.0"), MirrorStatus::Fresh);
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
