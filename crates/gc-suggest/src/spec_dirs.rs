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

use crate::embedded;

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
///   4. `~/.cache/ghost-complete/embedded-specs` (materialized lazily from
///      `gc_suggest::embedded::EMBEDDED_SPECS` via
///      [`embedded::materialize_embedded_specs`])
///
/// In the auto-detected path, the embedded directory is appended as the
/// lowest-precedence source, not only when every filesystem source is
/// missing. This closes both the `cargo install ghost-complete &&
/// ghost-complete` case and the Homebrew / installer-upgrade case where
/// `~/.config/ghost-complete/specs` exists but is stale and lacks specs
/// added by the newer binary. Explicit `paths.spec_dirs` remains an exact
/// override.
pub fn resolve_spec_dirs(configured: &[String]) -> Vec<PathBuf> {
    resolve_spec_dirs_with_embedded(
        configured,
        auto_detect_spec_dirs,
        embedded::materialize_embedded_specs,
    )
}

fn resolve_spec_dirs_with_embedded<A, E>(
    configured: &[String],
    auto_detect: A,
    materialize_embedded: E,
) -> Vec<PathBuf>
where
    A: FnOnce() -> Vec<PathBuf>,
    E: FnOnce() -> Option<PathBuf>,
{
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

    let mut dirs = auto_detect();

    if let Some(embedded_dir) = materialize_embedded() {
        if !dirs.iter().any(|dir| dir == &embedded_dir) {
            dirs.push(embedded_dir);
        }
    } else if dirs.is_empty() {
        tracing::warn!(
            "no spec directory available — autocomplete will fall back \
             to filesystem/history/$PATH only. Run `ghost-complete \
             install` to deploy the bundled completion specs."
        );
    }

    dirs
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

    /// Exercises the end-to-end chain the proxy hits: the embedded spec set
    /// must be reachable from `gc-suggest` and must load into a non-empty
    /// `SpecStore` via `load_from_dirs`. If `EMBEDDED_SPECS` is ever moved
    /// out of `gc-suggest`, or if the materialization helper stops actually
    /// writing files, this test will fail rather than silently regress
    /// autocomplete.
    #[test]
    fn embedded_fallback_yields_non_empty_spec_store() {
        // Materialize into a private tempdir rather than touching the user's
        // real `~/.cache/...`. This mirrors what
        // `embedded::materialize_embedded_specs` does internally and what
        // the spec loader will see when the auto-detection chain bottoms
        // out on a bare-install system.
        let tmp = tempfile::TempDir::new().unwrap();
        let count = embedded::write_embedded_specs(tmp.path()).unwrap();
        assert!(
            count > 0,
            "embedded spec set must contain at least one entry"
        );

        let result = crate::specs::SpecStore::load_from_dirs(&[tmp.path().to_path_buf()]).unwrap();
        assert!(
            !result.store.is_empty(),
            "SpecStore must be non-empty after loading from the embedded \
             fallback dir — empty here would mean the runtime fallback is \
             still broken"
        );
        // A few well-known commands every embedded set should contain. If
        // ALL three are missing the embedded set was truncated in transit.
        let known = ["git", "docker", "cargo"];
        assert!(
            known.iter().any(|cmd| result.store.get(cmd).is_some()),
            "expected at least one of {known:?} to be loaded from the \
             embedded fallback; the fallback may be empty"
        );
    }

    #[test]
    fn embedded_fallback_supplements_stale_installed_specs() {
        let installed = tempfile::TempDir::new().unwrap();
        let embedded = tempfile::TempDir::new().unwrap();
        std::fs::write(
            installed.path().join("z.json"),
            r#"{"name":"z","description":"stale installed copy"}"#,
        )
        .unwrap();
        std::fs::write(
            embedded.path().join("aws.json"),
            r#"{"name":"aws","subcommands":[{"name":"sso"}]}"#,
        )
        .unwrap();

        let dirs: Vec<String> = Vec::new();
        let installed_path = installed.path().to_path_buf();
        let embedded_path = embedded.path().to_path_buf();
        let resolved =
            resolve_spec_dirs_with_embedded(&dirs, || vec![installed_path], || Some(embedded_path));
        let result = crate::specs::SpecStore::load_from_dirs(&resolved).unwrap();

        assert!(
            result.store.get("z").is_some(),
            "installed specs should still be loaded"
        );
        assert!(
            result.store.get("aws").is_some(),
            "embedded specs must supplement stale installed specs so upgraded \
             binaries expose newly shipped specs without requiring \
             `ghost-complete install`"
        );
    }
}
