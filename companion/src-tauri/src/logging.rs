//! Uniform file-trace used across the companion. Every line gets an ISO
//! timestamp; the first line written from any process dumps [`BUILD_INFO`].
//!
//! The trace file lives at [`trace_path`] and is safe to `tail -f`.
//! On each process start we check the file size and rotate to a single
//! `<trace_path>.1` backup if it has grown past [`MAX_LOG_BYTES`] — keeps
//! disk usage bounded under the auto-resurrect / multi-mcp-stdio fan-out
//! without a daemon thread. Issue #L-2 in v0.4.10 review.
//!
//! Issue #185: the path used to be the hard-coded `/tmp/aiui-trace.log`,
//! which was wrong twice over. On Windows there is no `/tmp`, so the open
//! failed and **every trace line was silently dropped** — the platform
//! whose port is youngest shipped with no diagnostics at all. On Unix,
//! `/tmp` is world-writable and shared, so any local user could read the
//! trace or pre-plant the path as a symlink and have us append through it.
//! The file now lives under the per-user config dir (`%LOCALAPPDATA%` on
//! Windows), is opened `0600` with `O_NOFOLLOW`, and the resolved path is
//! named in the session header so bug reporters can find it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Resolved once per process: `<config_dir>/logs/aiui-trace.log`, with the
/// OS temp dir as a last resort if the config dir cannot be determined
/// (a trace we can still write beats no trace at all).
pub fn trace_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let base = log_dir_base();
        let dir = base.join("logs");
        // Keep the log dir private on Unix; best-effort, the open below is
        // what actually enforces the file mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            if !dir.exists() {
                let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
            }
        }
        #[cfg(not(unix))]
        let _ = fs::create_dir_all(&dir);
        dir.join("aiui-trace.log")
    })
    .as_path()
}

/// `%LOCALAPPDATA%\aiui` on Windows (machine-local: a log has no business
/// syncing into a roaming profile), `<config_dir()>` elsewhere.
fn log_dir_base() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(local) = dirs::data_local_dir() {
            return local.join("aiui");
        }
    }
    crate::config::config_dir().unwrap_or_else(|_| std::env::temp_dir().join("aiui"))
}

/// Rotation threshold. 4 MiB plain text holds ~30k trace lines — plenty
/// for a few weeks of normal use, well under macOS's `/tmp` cleanup
/// pressure. Past this we rotate to `aiui-trace.log.1` (single backup,
/// previous backup is dropped) so wall-clock logs never grow unbounded.
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

pub const BUILD_INFO: &str = concat!(
    "aiui v",
    env!("CARGO_PKG_VERSION"),
    " (build ",
    env!("AIUI_BUILD_TIMESTAMP"),
    " sha:",
    env!("AIUI_GIT_SHA"),
    ")"
);

static HEADER_WRITTEN: AtomicBool = AtomicBool::new(false);

/// Appends a trace line. On first call within a process, rotates the
/// log file if it's grown past [`MAX_LOG_BYTES`], then writes a header
/// naming the build + launch mode so every log session is
/// self-describing.
pub fn trace(msg: &str) {
    if !HEADER_WRITTEN.swap(true, Ordering::SeqCst) {
        rotate_if_needed();
        write_line(&format!(
            "---- {} started as {} pid={} log={} ----",
            BUILD_INFO,
            launch_mode(),
            std::process::id(),
            trace_path().display()
        ));
    }
    write_line(msg);
}

fn rotate_if_needed() {
    let path = trace_path();
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() < MAX_LOG_BYTES {
        return;
    }
    let backup = path.with_extension("log.1");
    // Best-effort rotation. If anything fails, the next write_line will
    // just append to the over-sized file — degraded but not broken.
    let _ = fs::remove_file(&backup);
    let _ = fs::rename(path, &backup);
}

fn launch_mode() -> &'static str {
    if std::env::args().any(|a| a == "--mcp-stdio") {
        "mcp-stdio"
    } else {
        "gui"
    }
}

/// One-shot guard so a permanently failing open cannot spam stderr — which,
/// for an `--mcp-stdio` child, shares a console with the host.
static OPEN_FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);

fn write_line(msg: &str) {
    let path = trace_path();
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 0600 on creation, and O_NOFOLLOW so a symlink planted at the path
        // fails the open instead of being appended through.
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match opts.open(path) {
        Ok(mut f) => {
            let now = chrono::Local::now();
            let _ = writeln!(f, "{} {}", now.format("%Y-%m-%d %H:%M:%S%.3f"), msg);
            let _ = f.flush();
        }
        Err(e) => {
            // Never swallow it silently: a dropped trace is exactly what made
            // the Windows blind spot invisible for a whole release.
            if !OPEN_FAILURE_REPORTED.swap(true, Ordering::SeqCst) {
                eprintln!("[aiui] cannot write trace to {}: {e}", path.display());
            }
        }
    }
}
