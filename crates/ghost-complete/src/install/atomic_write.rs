//! Atomic file replacement helper.
//!
//! Same-FS tempfile + rename ensures the target is never observed in a
//! partial state. If the target exists, its permissions are read first
//! and re-applied to the tempfile before rename; otherwise the new file
//! defaults to 0o644 (matching `fs::write`).
//!
//! This is the only path through which install/uninstall touch
//! `.zshrc`, `init.zsh`, or `ghost-complete.zsh` — those are the three
//! managed files whose torn writes would leave the user with a broken
//! shell hook sourced at the next prompt.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub(crate) fn atomic_write_preserving_mode(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    // Read existing mode if any; default to 0o644 only when the target does
    // not yet exist. Any other metadata failure (PermissionDenied on the
    // parent dir, broken symlink, EIO) is propagated rather than silently
    // widening permissions on the next rewrite — a 0o600 .zshrc must never
    // be downgraded to world-readable 0o644 because a stat briefly failed.
    let target_mode: u32 = match fs::metadata(path) {
        Ok(m) => m.permissions().mode() & 0o777,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0o644,
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!(
                "refusing to write {} without knowing its mode",
                path.display()
            )));
        }
    };

    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;

    // Apply target mode BEFORE persist so the rename publishes the
    // already-correct mode atomically.
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(target_mode))?;

    // Preserve the underlying io::Error chain so callers can downcast for
    // ErrorKind::PermissionDenied (the manual-instructions fallback path).
    // anyhow::anyhow!("string") would stringify the io::Error and break
    // callers that walk e.chain() to recover the original ErrorKind — see
    // `is_permission_denied` below, used by `install_to_with_cache_hooks`.
    tmp.persist(path).map_err(|e| {
        anyhow::Error::new(e.error).context(format!("atomic rename failed for {}", path.display()))
    })?;

    // Best-effort fsync parent for durability (no-op on APFS, but
    // load-bearing on filesystems where the dir entry is not committed
    // until the parent itself is synced — e.g. FAT on an external drive).
    // Failure is non-fatal; surface it under `RUST_LOG=debug` so an
    // operator can investigate rather than hitting an entirely silent loss.
    match fs::File::open(parent) {
        Ok(dir) => {
            if let Err(e) = dir.sync_all() {
                tracing::debug!("parent fsync failed for {}: {}", parent.display(), e);
            }
        }
        Err(e) => {
            tracing::debug!("could not open {} for fsync: {}", parent.display(), e);
        }
    }
    Ok(())
}

/// Returns true if `err` (or anything in its source chain) is an
/// `io::Error` with `ErrorKind::PermissionDenied`. Centralises the
/// chain-walk that `install_to_with_cache_hooks` relies on to take the
/// manual-instructions fallback when `.zshrc` (or its parent dir) is not
/// writable — e.g. on a nix-managed `~/.zshrc`. Callers that just want a
/// boolean should not have to re-derive the downcast inline; keeping the
/// walk here means the io::Error chain-preservation contract documented
/// on `atomic_write_preserving_mode` above lives next to the code that
/// establishes it.
pub(crate) fn is_permission_denied(err: &anyhow::Error) -> bool {
    // Scan every io::Error in the chain rather than stopping at the first.
    // `find_map` would return the kind of the topmost io::Error only, so a
    // future refactor that wraps a non-PermissionDenied io::Error around a
    // genuine PermissionDenied one would silently report `false` and fall
    // through to the catch-all in `install_to_with_cache_hooks`, breaking
    // the documented nix-managed `~/.zshrc` fallback.
    err.chain()
        .filter_map(|src| src.downcast_ref::<std::io::Error>())
        .any(|ioe| ioe.kind() == std::io::ErrorKind::PermissionDenied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn preserves_existing_file_mode() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("target");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        atomic_write_preserving_mode(&path, b"new").expect("write");

        let after = fs::read(&path).unwrap();
        assert_eq!(after, b"new");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "mode must be preserved");
    }

    #[test]
    fn creates_with_0644_when_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("target");

        atomic_write_preserving_mode(&path, b"new").expect("write");

        let after = fs::read(&path).unwrap();
        assert_eq!(after, b"new");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "new file must default to 0o644");
    }

    #[test]
    fn does_not_leak_tempfile_on_success() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("target");
        atomic_write_preserving_mode(&path, b"x").unwrap();
        let count = fs::read_dir(&dir).unwrap().count();
        assert_eq!(count, 1, "only the target should remain");
    }

    #[test]
    fn errors_on_path_with_no_parent() {
        // `/` has no parent — the early-return guard must surface a clear
        // anyhow error rather than panicking or leaking a tempfile in an
        // unrelated directory. Pin the behaviour so a future maintainer
        // can't accidentally remove the guard.
        let err = atomic_write_preserving_mode(Path::new("/"), b"x").unwrap_err();
        assert!(
            err.to_string().contains("path has no parent"),
            "expected `path has no parent` in error, got: {err}"
        );
    }

    #[test]
    fn preserves_io_error_kind_in_chain() {
        // The install fallback path at `install_to_with_cache_hooks`
        // walks `e.chain()` to recover the underlying io::Error kind
        // (`is_permission_denied`). A future refactor that switches the
        // `.persist()` arm to `anyhow::anyhow!("string")` would stringify
        // the io::Error and silently break that contract. Pin it.
        use std::io::ErrorKind;

        let dir = TempDir::new().unwrap();
        // Make the parent directory unwritable so `NamedTempFile::new_in`
        // fails with PermissionDenied.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        let target = dir.path().join("target");
        let err = atomic_write_preserving_mode(&target, b"x")
            .expect_err("writing under an unwritable parent must fail with PermissionDenied");

        // Restore perms so TempDir cleanup can remove the directory.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        let kind = err
            .chain()
            .find_map(|s| s.downcast_ref::<std::io::Error>())
            .map(|e| e.kind());
        assert_eq!(
            kind,
            Some(ErrorKind::PermissionDenied),
            "io::Error must remain in the chain so callers can downcast for kind; got chain: {err:?}"
        );
        assert!(
            is_permission_denied(&err),
            "is_permission_denied must agree with the manual chain walk"
        );
    }

    #[test]
    fn is_permission_denied_distinguishes_other_errors() {
        // A generic anyhow error (no io::Error in the chain) must not be
        // misclassified — otherwise the install fallback would silently
        // swallow real failures and print the manual-instructions block
        // when something unrelated went wrong.
        let other = anyhow::anyhow!("totally unrelated failure");
        assert!(!is_permission_denied(&other));

        let not_found = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(!is_permission_denied(&not_found));
    }

    #[test]
    fn is_permission_denied_finds_inner_permission_denied_through_wrapping_io_error() {
        // Regression pin for the chain-walk contract: when a non-PermissionDenied
        // io::Error wraps (or layers above) a PermissionDenied io::Error, the
        // helper must still report `true`. A naive `find_map` would terminate
        // at the first downcastable io::Error and miss a deeper PermissionDenied,
        // silently breaking the nix-managed `~/.zshrc` fallback in
        // `install_to_with_cache_hooks`.
        use std::io::{Error, ErrorKind};

        // Shape A: original PermissionDenied with a non-permission io::Error
        // layered on top via `.context()`. Today's chain shape across
        // `atomic_write_preserving_mode` is exactly this (context-wrapped
        // io::Error at the leaf), so this pins the realistic case.
        let outer = Error::other("outer-io");
        let err = anyhow::Error::new(Error::from(ErrorKind::PermissionDenied)).context(outer);
        assert!(
            is_permission_denied(&err),
            "outer non-permission io::Error must not mask a deeper PermissionDenied; got chain: {err:?}"
        );

        // Shape B: a custom error layer with a PermissionDenied io::Error as
        // its `source()`, then a string `.context()` on top. Exercises the
        // case where filter_map skips non-io chain links to reach the
        // PermissionDenied at the bottom.
        #[derive(Debug)]
        struct CustomLayer(Error);
        impl std::fmt::Display for CustomLayer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "custom-layer")
            }
        }
        impl std::error::Error for CustomLayer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let layered = CustomLayer(Error::from(ErrorKind::PermissionDenied));
        let err = anyhow::Error::new(layered).context("string context on top");
        assert!(
            is_permission_denied(&err),
            "PermissionDenied buried under a custom Error layer plus string context must still be detected; got chain: {err:?}"
        );
    }
}
