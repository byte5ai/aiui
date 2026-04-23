//! Uniform file-trace used across the companion. Every line gets an ISO
//! timestamp; the first line written from any process dumps [`BUILD_INFO`].
//!
//! The trace file lives at [`TRACE_PATH`] and is safe to `tail -f`.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

pub const TRACE_PATH: &str = "/tmp/aiui-trace.log";

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

/// Appends a trace line. On first call within a process, writes a header
/// naming the build + launch mode so every log session is self-describing.
pub fn trace(msg: &str) {
    if !HEADER_WRITTEN.swap(true, Ordering::SeqCst) {
        write_line(&format!(
            "---- {} started as {} pid={} ----",
            BUILD_INFO,
            launch_mode(),
            std::process::id()
        ));
    }
    write_line(msg);
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
