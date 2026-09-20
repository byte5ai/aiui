//! Small file-system utilities shared across modules.
//!
//! The only thing here today is `atomic_write`. Everything else stays
//! in its caller.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Follow a symlink chain to the file the path really designates.
///
/// Issue #185: every user config aiui rewrites goes through `atomic_write`,
/// and a `rename` over a symlink **replaces the symlink itself** with a
/// regular file. `~/.claude.json` or `~/.ssh/config` symlinked into a
/// dotfiles repo — a common setup — would be silently detached from it.
/// Resolving first means we write the temp next to, and rename over, the
/// real file, so the link survives.
///
/// Deliberately not `fs::canonicalize`: that requires the destination to
/// exist, while first-write callers legitimately pass a path that does not.
///
/// The hop count is bounded, and exhausting it is an **error**, not a
/// fallback. `rename` does not follow symlinks — it replaces them — so
/// returning the unresolved path on a cycle would quietly swap the broken
/// link for a regular file, which is the very data loss this function
/// exists to prevent. A cycle is a broken state the caller should hear
/// about.
fn resolve_symlink(path: &Path) -> std::io::Result<PathBuf> {
    let mut cur = path.to_path_buf();
    for _ in 0..32 {
        let is_link = fs::symlink_metadata(&cur)
            .map(|md| md.file_type().is_symlink())
            .unwrap_or(false);
        if !is_link {
            return Ok(cur);
        }
        match fs::read_link(&cur) {
            Ok(target) => {
                cur = if target.is_absolute() {
                    target
                } else {
                    cur.parent().unwrap_or_else(|| Path::new(".")).join(target)
                };
            }
            // Unreadable link: hand back what we have and let the write
            // surface the real error against it.
            Err(_) => return Ok(cur),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "too many symlink hops resolving {} (cycle?)",
            path.display()
        ),
    ))
}

/// Atomically write `content` to `path`: write to a sibling temp file
/// first, fsync it, then rename over the destination. A crash or kill
/// mid-write leaves either the old file (rename hasn't happened yet) or
/// the new file (rename completed) — never a half-written/corrupted
/// destination. Issue #M-2 in v0.4.10 review.
///
/// Preserves the destination's permission bits and symlink identity — see
/// [`atomic_write_with_mode`], of which this is the no-default-mode form.
pub fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    atomic_write_with_mode(path, content, None)
}

/// [`atomic_write`] plus an explicit mode for the **create** case.
///
/// Issue #185: the temp file is created fresh (`0o666` masked by the
/// process umask) and renamed over the destination, so without this the
/// destination's own mode is silently discarded. `~/.claude.json` is
/// created `0600` by Claude Code because it holds OAuth data and other MCP
/// servers' `env` blocks; after one aiui config patch it came back `0644`.
/// `~/.ssh/config` under a `002` umask became group-writable, which makes
/// OpenSSH refuse to run at all.
///
/// Rules, applied to the symlink-resolved destination:
/// - it exists → copy its current mode onto the temp handle;
/// - it does not exist → apply `default_mode` (`0o600` for token-like
///   files, `0o644` for configs). `None` falls back to the umask.
///
/// The mode is set **on the open handle, before the rename**, so the file
/// is never briefly world-readable under its final name. Do not "fix" this
/// by chmod'ing after the rename: that leaves the window open and does
/// nothing at all for a file that already exists.
pub fn atomic_write_with_mode(
    path: &Path,
    content: &[u8],
    default_mode: Option<u32>,
) -> std::io::Result<()> {
    // Bind on non-Unix so the param isn't flagged unused (clippy -D warnings
    // on the Windows target). Windows files inherit the directory's ACL.
    #[cfg(not(unix))]
    let _ = default_mode;

    let dest = resolve_symlink(path)?;
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    // Sibling temp file so the rename stays on the same filesystem
    // (cross-fs rename would degrade to copy+delete and lose atomicity).
    // PID + nanos make the path unique enough that two concurrent writers
    // to the same target won't trample each other's temp files.
    let tmp = dest.with_extension(format!(
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&dest)
                .ok()
                .map(|md| md.permissions().mode() & 0o7777)
                .or(default_mode);
            if let Some(m) = mode {
                let _ = f.set_permissions(fs::Permissions::from_mode(m));
            }
        }
        if let Err(e) = f.write_all(content) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        f.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, &dest) {
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

    #[cfg(unix)]
    #[test]
    fn preserves_destination_mode() {
        // Issue #185: ~/.claude.json is 0600 (OAuth data). A config patch
        // must not hand it back world-readable.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aiui-fsutil-mode-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("claude.json");
        atomic_write(&target, b"{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        atomic_write(&target, b"{\"patched\": true}").unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "destination mode survives the rewrite");
        assert_eq!(fs::read_to_string(&target).unwrap(), "{\"patched\": true}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn applies_default_mode_only_when_creating() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aiui-fsutil-def-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("token");
        // Fresh file: the default applies.
        atomic_write_with_mode(&target, b"deadbeef", Some(0o600)).unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Existing file with a tighter mode: the destination wins, not the
        // default — we never loosen what the user (or we) already set.
        fs::set_permissions(&target, fs::Permissions::from_mode(0o400)).unwrap();
        atomic_write_with_mode(&target, b"cafebabe", Some(0o644)).unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o400, "existing mode is preserved over the default");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_a_symlink_instead_of_replacing_it() {
        // Issue #185: ~/.claude.json symlinked into a dotfiles repo must stay
        // a symlink — a rename over it would detach the file from the repo.
        let dir = std::env::temp_dir().join(format!("aiui-fsutil-link-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let real = dir.join("real.json");
        let link = dir.join("link.json");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        atomic_write(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
            "the symlink itself survives"
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "new", "target updated");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cycle_terminates() {
        // A cycle must not hang the caller; the write fails honestly instead.
        let dir = std::env::temp_dir().join(format!("aiui-fsutil-cyc-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let a = dir.join("a");
        let b = dir.join("b");
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();
        assert!(atomic_write(&a, b"x").is_err());
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
