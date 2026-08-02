//! Small file-system utilities shared across modules.
//!
//! The only thing here today is `atomic_write`. Everything else stays
//! in its caller.

use std::fs;
use std::io::Write;
use std::path::Path;

/// Atomically write `content` to `path`: write to a sibling temp file
/// first, fsync it, then rename over the destination. A crash or kill
/// mid-write leaves either the old file (rename hasn't happened yet) or
/// the new file (rename completed) — never a half-written/corrupted
/// destination. Issue #M-2 in v0.4.10 review.
pub fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    // Sibling temp file so the rename stays on the same filesystem
    // (cross-fs rename would degrade to copy+delete and lose atomicity).
    // PID + nanos make the path unique enough that two concurrent writers
    // to the same target won't trample each other's temp files.
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Like [`atomic_write`], but never overwrites an existing file. Writes
/// `content` to a sibling temp, fsyncs it, then hard-links it into place:
/// the link is atomic and fails with [`std::io::ErrorKind::AlreadyExists`]
/// if the destination already exists, closing the check-then-write race a
/// plain `exists()` guard leaves open. The temp is always cleaned up. Used
/// by the upload tool, whose contract is a deterministic destination that
/// must not clobber the user's data even under a concurrent create (codex
/// review P2).
pub fn write_new(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
    }
    let res = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces() {
        let dir = std::env::temp_dir().join(format!("aiui-fsutil-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("a.txt");
        atomic_write(&target, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
        atomic_write(&target, b"world").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "world");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_new_refuses_to_clobber() {
        let dir = std::env::temp_dir().join(format!("aiui-fsutil-wn-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("b.txt");
        write_new(&target, b"first").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "first");
        // Second write to an existing path must fail atomically, not clobber.
        let err = write_new(&target, b"second").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&target).unwrap(), "first");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_temp_files_left_behind_on_success() {
        let dir = std::env::temp_dir().join(format!(
            "aiui-fsutil-leftover-{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("b.txt");
        atomic_write(&target, b"x").unwrap();
        let entries: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "only the target file should remain");
        let _ = fs::remove_dir_all(&dir);
    }
}
