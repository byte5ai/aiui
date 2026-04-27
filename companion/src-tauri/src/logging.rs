//! Uniform file-trace used across the companion. Every line gets an ISO
//! timestamp; the first line written from any process dumps [`BUILD_INFO`].
//!
//! The trace file lives at [`TRACE_PATH`] and is safe to `tail -f`.
//! On each process start we check the file size and rotate to a single
//! `<TRACE_PATH>.1` backup if it has grown past [`MAX_LOG_BYTES`] — keeps
//! disk usage bounded under the auto-resurrect / multi-mcp-stdio fan-out
//! without a daemon thread. Issue #L-2 in v0.4.10 review.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

pub const TRACE_PATH: &str = "/tmp/aiui-trace.log";

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
            "---- {} started as {} pid={} ----",
            BUILD_INFO,
            launch_mode(),
            std::process::id()
        ));
    }
    write_line(msg);
}

fn rotate_if_needed() {
    let Ok(meta) = fs::metadata(TRACE_PATH) else {
        return;
    };
    if meta.len() < MAX_LOG_BYTES {
        return;
    }
    let backup = format!("{TRACE_PATH}.1");
    // Best-effort rotation. If anything fails, the next write_line will
    // just append to the over-sized file — degraded but not broken.
    let _ = fs::remove_file(&backup);
    let _ = fs::rename(TRACE_PATH, &backup);
}

fn launch_mode() -> &'static str {
    if std::env::args().any(|a| a == "--mcp-stdio") {
        "mcp-stdio"
    } else {
        "gui"
    }
}

fn write_line(msg: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(TRACE_PATH)
    {
        let now = chrono::Local::now();
        let _ = writeln!(f, "{} {}", now.format("%Y-%m-%d %H:%M:%S%.3f"), msg);
        let _ = f.flush();
    }
}
