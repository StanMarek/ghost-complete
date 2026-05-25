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

// Task 9 will wire this into the install/uninstall call sites. The
// helper lands one commit ahead so the test suite can exercise the
// mode-preservation contract in isolation; suppress the dead-code lint
// until the callers arrive.
#[allow(dead_code)]
pub fn atomic_write_preserving_mode(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    // Read existing mode if any; default to 0o644 for new files.
    let target_mode: u32 = fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644);

    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;

    // Apply target mode BEFORE persist so the rename publishes the
    // already-correct mode atomically.
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(target_mode))?;

    // Preserve the underlying io::Error chain so callers can downcast for
    // ErrorKind::PermissionDenied (the manual-instructions fallback path).
    // anyhow::anyhow!("string") would stringify the io::Error and break the
    // downcast at install.rs:522.
    tmp.persist(path).map_err(|e| {
        anyhow::Error::new(e.error).context(format!("atomic rename failed for {}", path.display()))
    })?;

    // Best-effort fsync parent for durability (no-op on APFS).
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
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
}
