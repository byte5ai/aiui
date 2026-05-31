//! Media cache for the gallery/form video feature (2026-05-31).
//!
//! ## Why this exists
//!
//! Images inline fine as `data:` URLs (the bridge-side resolvers do that).
//! Video does not: a 50 MB clip is 67 MB of base64 in the render spec — it
//! chokes the `get_dialog_spec` IPC and pins that much in the dialog
//! registry. And a *remote* agent's local file isn't readable from the Mac
//! at all — the only channel between them is the SSH **reverse** tunnel
//! (remote → Mac on :7777). There is no Mac → remote forward (proven
//! empirically; Claude Desktop provides none), so `scp` from the Mac is not
//! an option.
//!
//! ## How it works
//!
//! The bridge (running on whichever host holds the file) **pushes** the
//! bytes to the Mac over the existing :7777 channel: `POST /media`. The Mac
//! stores them under its app cache dir and serves them back to the dialog
//! WebView via `GET /media/blob/<file>` (range-capable, through
//! `tower_http::services::ServeDir`). Because the reverse tunnel maps
//! `remote:7777 → mac:7777`, the very same `http://127.0.0.1:7777/...`
//! playback URL the upload returns is valid both on the remote (where the
//! bridge runs) and on the Mac (where the WebView plays it).
//!
//! Serving is an unauthenticated **capability URL**: the filename is a v4
//! UUID, unguessable, and the server only binds loopback (+ the user's own
//! reverse tunnel). Uploads require the bearer token like every other
//! mutating endpoint.
//!
//! ## Eviction
//!
//! The cache is bounded two ways, swept on every upload and once at startup:
//! a per-file TTL (stale clips vanish even if the app never restarts) and a
//! total-size cap (oldest-first deletion when the sum is exceeded). The
//! cache is disposable — a missing file just renders as a broken `<video>`,
//! never a crash — so the eviction is best-effort and never blocks a render.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// How long a cached media file lives before the sweep removes it. Matches
/// the dialog TTL — a clip is only ever needed while its dialog is open, and
/// dialogs themselves expire at 2 h.
pub const MEDIA_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// Total cache-size ceiling. When an upload pushes the directory past this,
/// the oldest files are deleted (by mtime) until it fits again. 1 GiB holds
/// a healthy batch of review clips without letting a runaway session fill
/// the user's disk.
pub const MEDIA_TOTAL_CAP: u64 = 1024 * 1024 * 1024;

/// Largest single upload accepted. Enforced at the HTTP layer via
/// `DefaultBodyLimit`; duplicated here as the documented contract.
pub const MEDIA_FILE_CAP: u64 = 512 * 1024 * 1024;

/// The cache directory: `<app-cache-dir>/media`, created if absent.
pub fn media_dir(app: &AppHandle) -> std::io::Result<PathBuf> {
    let base = app
        .path()
        .app_cache_dir()
        .map_err(|e| std::io::Error::other(format!("no app cache dir: {e}")))?;
    let dir = base.join("media");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Keep only `[a-z0-9]`, lowercased, max 5 chars; fall back to `bin`. The
/// extension is attacker-influenced (it comes off the wire), and it ends up
/// in a filename *and* drives the served `Content-Type`, so it must not
/// carry path separators, dots, or anything exotic.
pub fn sanitize_ext(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(5)
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        "bin".to_string()
    } else {
        cleaned
    }
}

/// Write `bytes` to a fresh `<uuid>.<ext>` file under `dir`. Returns the
/// filename (the capability id used in the `/media/blob/<file>` URL).
pub fn store(dir: &Path, bytes: &[u8], ext: &str) -> std::io::Result<String> {
    let name = format!("{}.{}", Uuid::new_v4(), sanitize_ext(ext));
    let path = dir.join(&name);
    std::fs::write(&path, bytes)?;
    Ok(name)
}

/// Best-effort eviction. Removes files older than `ttl`, then — if the
/// remaining total still exceeds `total_cap` — deletes oldest-first until it
/// fits. Errors on individual files are swallowed (a locked/just-deleted
/// file must never abort a render); the function logs nothing on the hot
/// path by design.
pub fn sweep(dir: &Path, ttl: Duration, total_cap: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // (path, mtime, size) for every regular file in the cache.
    let mut files: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(now);
        // TTL pass: drop anything past its lifetime immediately.
        if let Ok(age) = now.duration_since(mtime) {
            if age > ttl {
                let _ = std::fs::remove_file(&path);
                continue;
            }
        }
        files.push((path, mtime, meta.len()));
    }
    // Size pass: if still over the cap, evict oldest first.
    let mut total: u64 = files.iter().map(|(_, _, sz)| *sz).sum();
    if total <= total_cap {
        return;
    }
    files.sort_by_key(|(_, mtime, _)| *mtime); // oldest first
    for (path, _, sz) in files {
        if total <= total_cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(sz);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_ext_strips_junk_and_caps_length() {
        assert_eq!(sanitize_ext("mp4"), "mp4");
        assert_eq!(sanitize_ext(".MOV"), "mov");
        assert_eq!(sanitize_ext("../../etc/passwd"), "etcpa"); // separators gone, capped at 5
        assert_eq!(sanitize_ext(""), "bin");
        assert_eq!(sanitize_ext("..."), "bin");
        assert_eq!(sanitize_ext("we!b@m#"), "webm");
    }

    #[test]
    fn store_writes_uuid_named_file_with_ext() {
        let dir = std::env::temp_dir().join(format!("aiui-media-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = store(&dir, b"hello", "mp4").unwrap();
        assert!(name.ends_with(".mp4"));
        let content = std::fs::read(dir.join(&name)).unwrap();
        assert_eq!(content, b"hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sweep_evicts_over_total_cap_oldest_first() {
        let dir = std::env::temp_dir().join(format!("aiui-media-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Three 100-byte files; cap at 250 should leave the two newest.
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        let c = dir.join("c.bin");
        std::fs::write(&a, vec![0u8; 100]).unwrap();
        // Stagger mtimes so ordering is deterministic.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&b, vec![0u8; 100]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&c, vec![0u8; 100]).unwrap();

        sweep(&dir, MEDIA_TTL, 250);

        assert!(!a.exists(), "oldest should be evicted");
        assert!(b.exists(), "newer survives");
        assert!(c.exists(), "newest survives");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sweep_removes_files_past_ttl() {
        let dir = std::env::temp_dir().join(format!("aiui-media-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("old.bin");
        std::fs::write(&f, b"x").unwrap();
        // TTL of zero → everything is already stale.
        sweep(&dir, Duration::from_secs(0), MEDIA_TOTAL_CAP);
        assert!(!f.exists(), "file past TTL should be removed");
        std::fs::remove_dir_all(&dir).ok();
    }
}
