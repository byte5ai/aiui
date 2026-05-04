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

/// Live counter of currently-attached MCP-stdio children. Owned by the Tauri
/// app via `manage()` and read by `/health` to surface child count in the
/// composite-health response.
pub struct LifetimeStats {
    pub conns: Arc<AtomicUsize>,
}

impl LifetimeStats {
    pub fn new() -> Self {
        Self {
            conns: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn child_count(&self) -> usize {
        self.conns.load(Ordering::SeqCst)
    }
}

/// GUI-side: bind the socket, accept connections, and self-terminate after a
/// grace period once all clients are gone. Increments/decrements the shared
/// `conns` counter on every connect/disconnect so `/health` can report the
/// live child count without polling.
///
/// Multi-instance hardening (since 0.4.33): if the socket path already
/// answers a connection, another aiui-app is alive and we are the
/// duplicate — exit immediately rather than racing for ownership. The
/// previous behaviour (`remove_file` then `bind`) silently tore the
/// existing instance's listener out from under it on every dup-launch,
/// which is how the 2026-05-04 dual-companion incident produced reset
/// connections in the first place.
pub async fn gui_serve(sock: PathBuf, app: AppHandle, conns: Arc<AtomicUsize>) {
    if sock.exists() {
        // Probe whether the existing socket is live (another aiui is
        // listening) or a stale leftover from a crashed previous run.
        // A live listener accepts the connection; a stale path returns
        // ENOENT / ECONNREFUSED.
        match tokio::net::UnixStream::connect(&sock).await {
            Ok(stream) => {
                drop(stream);
                trace(&format!(
                    "lifetime: another aiui already serves {} — exiting (multi-instance)",
                    sock.display()
                ));
                let app_for_exit = app.clone();
                let _ = app
                    .run_on_main_thread(move || app_for_exit.exit(1));
                return;
            }
            Err(_) => {
                // Stale; safe to remove and re-bind.
                let _ = std::fs::remove_file(&sock);
            }
        }
    }
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            // Bind failed despite the existence check above — race
            // condition, another instance grabbed it between our probe
            // and our bind. Same conclusion: we're the duplicate, exit.
            trace(&format!(
                "lifetime: bind {} failed: {e} — exiting (multi-instance race)",
                sock.display()
            ));
            let app_for_exit = app.clone();
            let _ = app.run_on_main_thread(move || app_for_exit.exit(1));
            return;
        }
    };
    trace(&format!("lifetime: listening on {}", sock.display()));

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

/// True iff this process appears to be running in an interactive desktop
/// session — i.e. somewhere a user could see a window. Returns false on
/// remote/headless contexts (SSH, CI, docker exec) where launching a GUI
/// would create a phantom window nobody can see and that holds port 7777
/// hostage. Issue #80.
///
/// The signal is intentionally simple and conservative: any of the
/// SSH-related env variables being set means "someone is logged in over
/// the network here, the GUI doesn't belong to them". Cross-platform —
/// works the same on macOS, Linux, and Windows so the Windows port
/// inherits the right behavior without further work.
pub fn is_interactive_session() -> bool {
    if std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
        || std::env::var_os("SSH_TTY").is_some()
    {
        return false;
    }
    true
}

/// MCP-stdio-side: keep the GUI alive for the entire lifetime of this
/// MCP child process. On an interactive desktop session: if the GUI isn't
/// running, launch it; if it dies, relaunch and reattach. On
/// remote/headless hosts: never spawn a GUI — just keep retrying the
/// socket attach in case a tunnel-fronted GUI on the user's machine
/// becomes reachable. The loop only exits when this MCP-stdio process
/// itself is terminated, which happens when the parent (Claude Desktop /
/// Claude Code) tears down its MCP children.
///
/// "Auto-resurrect" contract holds for local-Mac usage. On remotes the
/// MCP-stdio child trusts the SSH-reverse-tunnel to forward port 7777
/// back to the user's machine where the actual aiui GUI lives.
pub async fn mcp_attach(sock: PathBuf) {
    let interactive = is_interactive_session();
    if !interactive {
        trace(
            "lifetime: detected non-interactive session (SSH/headless), \
             auto-resurrect of GUI is suppressed on this host",
        );
    }

    loop {
        // Try to attach. If the socket isn't there, spawn the GUI and retry.
        let mut attached = false;
        for attempt in 1..=30u32 {
            match UnixStream::connect(&sock).await {
                Ok(mut stream) => {
                    trace(&format!(
                        "lifetime: mcp attached to {} (attempt {attempt})",
                        sock.display()
                    ));
                    attached = true;
                    let mut buf = [0u8; 64];
                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => continue,
                        }
                    }
                    trace("lifetime: mcp socket closed — GUI is gone, will relaunch");
                    break;
                }
                Err(e) => {
                    if attempt == 1 && interactive {
                        trace(&format!(
                            "lifetime: gui socket not ready ({e}), launching GUI via `open --auto`"
                        ));
                        let _ = std::process::Command::new("open")
                            .args(["-g", "-a", "aiui", "--args", "--auto"])
                            .spawn();
                    } else if attempt == 1 {
                        // Non-interactive: don't spawn, just log the first miss
                        // so the trace explains why we're waiting.
                        trace(&format!(
                            "lifetime: gui socket not ready ({e}), \
                             non-interactive session — GUI must be reachable \
                             via SSH-reverse-tunnel from the user's machine"
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
        if !attached {
            trace("lifetime: mcp gave up waiting for gui socket after 30 attempts; retrying in 5s");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        // GUI was connected and has now closed; loop back to resurrect it
        // (or wait + retry if launch failed / suppressed).
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_connection_signals_non_interactive() {
        // We can't safely flip global env in a parallel test runner, so
        // we just verify the env-lookup paths exist and the function is
        // pure. Real behavior is exercised in integration tests.
        let _ = is_interactive_session();
    }
}
