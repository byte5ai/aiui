//! Lifetime coupling between the GUI process and MCP-stdio children.
//!
//! The GUI hosts a Unix domain socket at `<config>/gui.sock`. Each
//! `aiui --mcp-stdio` child connects on startup and holds the stream open.
//! When the child exits (Claude Desktop closes it), the OS tears down the
//! stream and the GUI observes an EOF. Once the last client disconnects the
//! GUI starts a 60s grace timer and exits if nobody re-connects.
//!
//! Event-driven, no polling.

use crate::logging::trace;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;

pub const SHUTDOWN_GRACE_SECS: u64 = 60;

pub fn socket_path(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("gui.sock")
}

/// GUI-side: bind the socket, accept connections, and self-terminate after a
/// grace period once all clients are gone.
pub async fn gui_serve(sock: PathBuf, app: AppHandle) {
    let _ = std::fs::remove_file(&sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            trace(&format!("lifetime: bind {} failed: {e}", sock.display()));
            return;
        }
    };
    trace(&format!("lifetime: listening on {}", sock.display()));

    let conns = Arc::new(AtomicUsize::new(0));
    let wake = Arc::new(Notify::new());

    // Shutdown watcher — armed every time conns hits 0.
    {
        let conns = conns.clone();
        let wake = wake.clone();
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                wake.notified().await;
                if conns.load(Ordering::SeqCst) > 0 {
                    continue;
                }
                trace(&format!(
                    "lifetime: no clients, grace timer {SHUTDOWN_GRACE_SECS}s"
                ));
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(SHUTDOWN_GRACE_SECS)) => {
                        if conns.load(Ordering::SeqCst) == 0 {
                            trace("lifetime: grace expired, exiting");
                            // Hard exit bypassing Tauri's ExitRequested dance —
                            // Cmd-Q and window-close are deliberately blocked
                            // there, so the only legitimate shutdown path is
                            // this one.
                            let _ = app;
                            std::process::exit(0);
                        }
                    }
                    _ = wake.notified() => {
                        trace("lifetime: new client within grace, staying");
                    }
                }
            }
        });
    }

    loop {
        match listener.accept().await {
            Ok((mut stream, _)) => {
                let n = conns.fetch_add(1, Ordering::SeqCst) + 1;
                trace(&format!("lifetime: client connected, active={n}"));
                let conns = conns.clone();
                let wake = wake.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => continue,
                        }
                    }
                    let left = conns.fetch_sub(1, Ordering::SeqCst) - 1;
                    trace(&format!("lifetime: client disconnected, active={left}"));
                    if left == 0 {
                        wake.notify_one();
                    }
                });
            }
            Err(e) => {
                trace(&format!("lifetime: accept error: {e}"));
            }
        }
    }
}

/// MCP-stdio-side: connect to the GUI socket, launching the GUI first if the
/// socket isn't there yet. Hold the connection open until the process exits.
pub async fn mcp_attach(sock: PathBuf) {
    let mut launched = false;
    for attempt in 1..=30u32 {
        match UnixStream::connect(&sock).await {
            Ok(mut stream) => {
                trace(&format!(
                    "lifetime: mcp attached to {} (attempt {attempt})",
                    sock.display()
                ));
                let mut buf = [0u8; 64];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => continue,
                    }
                }
                trace("lifetime: mcp socket closed");
                return;
            }
            Err(e) => {
                if !launched {
                    trace(&format!(
                        "lifetime: gui socket not ready ({e}), launching GUI via `open --auto`"
                    ));
                    let _ = std::process::Command::new("open")
                        .args(["-g", "-a", "aiui", "--args", "--auto"])
                        .spawn();
                    launched = true;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    trace("lifetime: mcp gave up waiting for gui socket after 30 attempts");
}
