//! Shared spec directory resolution.
//!
//! Historically the PTY proxy (`gc-pty::proxy`) and the CLI commands in
//! `ghost-complete` (status / validate-specs) each had their own copy of
//! spec-dir resolution. The proxy version validated `is_dir()`, fell back
//! through a 3-step chain (config dir → next-to-binary → cwd → default
//! `specs/`), and emitted per-path `tracing::warn!` lines for invalid
//! entries. The CLI version did none of that, so `ghost-complete status`
//! and `validate-specs` displayed a different set of spec dirs than the
//! proxy actually loaded.
//!
//! This module is the single source of truth. Both crates call
//! [`resolve_spec_dirs`] to get the same behavior.

use std::path::PathBuf;

/// Partition result from [`partition_spec_dirs`]: tilde-expanded valid
/// directories and the raw (pre-expansion) strings for entries that don't
/// resolve to an existing directory.
pub struct SpecDirPartition {
    pub valid: Vec<PathBuf>,
    pub invalid: Vec<String>,
}

/// Partition configured spec_dirs into valid/invalid entries after tilde
/// expansion.
///
/// A path is valid iff it resolves to an existing directory on disk. The
/// `invalid` vector preserves the raw configured strings (pre-expansion) in
/// input order so callers can log warnings that match what the user wrote
/// in their config file.
pub fn partition_spec_dirs(configured: &[String]) -> SpecDirPartition {
    let mut valid: Vec<PathBuf> = Vec::with_capacity(configured.len());
    let mut invalid: Vec<String> = Vec::new();
    for raw in configured {
        let path = expand_tilde(raw);
        if path.is_dir() {
            valid.push(path);
        } else {
            invalid.push(raw.clone());
        }
    }
    SpecDirPartition { valid, invalid }
}

/// Resolve spec directories from config, with tilde expansion.
///
/// If `configured` is non-empty, validate each entry and use the valid
/// subset exactly; emit a `tracing::warn!` for each invalid entry. If
/// every configured entry is invalid, fall through to auto-detection.
///
/// Auto-detection chain (accumulates existing filesystem directories in
/// this order):
///   1. `~/.config/ghost-complete/specs` (installed by `ghost-complete install`)
///   2. `<current_exe_dir>/specs` (development / `cargo run`)
///   3. `./specs` (cwd, development)
///
/// The runtime fallback path no longer materialises embedded specs to
/// `~/.cache/ghost-complete/embedded-specs/`. The embedded corpus is
/// consumed in-memory by [`crate::specs::SpecStore::load_with_embedded`]
/// instead — same registration semantics as a filesystem dir, no disk
/// I/O. Explicit `paths.spec_dirs` remains an exact override.
pub fn resolve_spec_dirs(configured: &[String]) -> Vec<PathBuf> {
    if !configured.is_empty() {
        let partition = partition_spec_dirs(configured);
        for bad in &partition.invalid {
            tracing::warn!(
                configured = %bad,
                resolved = %expand_tilde(bad).display(),
                "configured spec_dir is not a directory, skipping"
            );
        }
        if !partition.valid.is_empty() {
            return partition.valid;
        }
        tracing::warn!("all configured spec_dirs are invalid — falling back to auto-detection");
    }

    auto_detect_spec_dirs()
}

fn auto_detect_spec_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // Config directory (installed by `ghost-complete install`)
    if let Some(config_dir) = gc_config::config_dir() {
        let spec_dir = config_dir.join("specs");
        if spec_dir.is_dir() {
            dirs.push(spec_dir);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let spec_dir = exe_dir.join("specs");
            if spec_dir.is_dir() {
                dirs.push(spec_dir);
            }
        }
    }

    // Fall back to specs/ in the current directory (development)
    let cwd_specs = PathBuf::from("specs");
    if cwd_specs.is_dir() {
        dirs.push(cwd_specs);
    }

    dirs
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_spec_dirs_separates_valid_and_invalid() {
        // "." is always a directory; the fake paths never are. This keeps
        // the test dependency-free while still exercising every branch.
        let configured = vec![
            ".".to_string(),
            "/ghost-complete-nonexistent-xyz-1".to_string(),
            "/ghost-complete-nonexistent-xyz-2".to_string(),
        ];
        let partition = partition_spec_dirs(&configured);
        assert_eq!(partition.valid.len(), 1, "expected only `.` to be valid");
        assert_eq!(partition.valid[0], PathBuf::from("."));
        assert_eq!(
            partition.invalid,
            vec![
                "/ghost-complete-nonexistent-xyz-1".to_string(),
                "/ghost-complete-nonexistent-xyz-2".to_string(),
            ],
            "invalid list must preserve raw configured strings in input order"
        );
    }

    #[test]
    fn partition_spec_dirs_empty_input() {
        let partition = partition_spec_dirs(&[]);
        assert!(partition.valid.is_empty());
        assert!(partition.invalid.is_empty());
    }

    #[test]
    fn partition_spec_dirs_all_valid() {
        let configured = vec![".".to_string()];
        let partition = partition_spec_dirs(&configured);
        assert_eq!(partition.valid, vec![PathBuf::from(".")]);
        assert!(partition.invalid.is_empty());
    }

    #[test]
    fn partition_spec_dirs_all_invalid() {
        let configured = vec!["/ghost-complete-fake-path-zzz".to_string()];
        let partition = partition_spec_dirs(&configured);
        assert!(partition.valid.is_empty());
        assert_eq!(
            partition.invalid,
            vec!["/ghost-complete-fake-path-zzz".to_string()]
        );
    }

    /// Filesystem-installed specs win precedence over the embedded
    /// corpus when both ship the same filename. Pre-v0.12.4 this was
    /// expressed by appending the materialised embedded dir to the
    /// auto-detected list; v0.12.4 onward the embedded corpus is
    /// loaded in-memory by `SpecStore::load_with_embedded`, but the
    /// precedence semantics are identical.
    #[test]
    fn installed_specs_win_precedence_over_embedded_via_load_with_embedded() {
        let installed = tempfile::TempDir::new().unwrap();
        std::fs::write(
            installed.path().join("git.json"),
            r#"{"name":"git","subcommands":[{"name":"installed-copy"}]}"#,
        )
        .unwrap();

        let result = crate::specs::SpecStore::load_with_embedded(&[installed
            .path()
            .to_path_buf()])
        .unwrap();
        let git = result.store.get("git").expect("git spec must load");
        assert_eq!(
            git.subcommands[0].name, "installed-copy",
            "installed specs must keep precedence when embedded ships the \
             same filename"
        );
    }

    /// `load_with_embedded` with no filesystem dirs registers the
    /// binary's embedded corpus directly. If `EMBEDDED_SPECS` is ever
    /// moved out of `gc-suggest`, or if the in-memory wiring breaks,
    /// this test fails rather than silently regress autocomplete.
    #[test]
    fn embedded_corpus_yields_non_empty_spec_store() {
        let result = crate::specs::SpecStore::load_with_embedded(&[]).unwrap();
        assert!(
            !result.store.is_empty(),
            "SpecStore must be non-empty after loading the embedded corpus"
        );
        // A few well-known commands every embedded set should contain.
        let known = ["git", "docker", "cargo"];
        assert!(
            known.iter().any(|cmd| result.store.get(cmd).is_some()),
            "expected at least one of {known:?} to be loaded from the \
             embedded corpus; the corpus may be empty"
        );
    }
}
