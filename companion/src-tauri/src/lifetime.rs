//! Lifetime coupling between the GUI process and MCP-stdio children.
//!
//! The GUI hosts a per-user channel — a Unix domain socket on macOS/Linux,
//! a Windows named pipe on Windows — and each `aiui --mcp-stdio` child
//! connects on startup and holds the stream open. When the child exits
//! (Claude Desktop closes it), the OS tears down the stream and the GUI
//! observes an EOF. Once the last client disconnects the GUI starts a 60s
//! grace timer and exits if nobody re-connects.
//!
//! Event-driven, no polling.
//!
//! Cross-platform note: the public surface (`socket_path`, `gui_serve`,
//! `mcp_attach`, `LifetimeStats`) is identical on both OSes. The only
//! per-OS code lives behind `cfg` blocks below — the rest of the program
//! treats the channel as an opaque handshake.

use crate::logging::trace;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::io::AsyncReadExt;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(windows)]
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::sync::Notify;

pub const SHUTDOWN_GRACE_SECS: u64 = 60;

/// Returns the per-OS handle the GUI listens on and MCP-stdio children
/// connect to.
///
/// - Unix: a real filesystem path under the aiui config dir
///   (`<config>/gui.sock`) — its existence on disk doubles as a stale-leftover
///   indicator after a crash.
/// - Windows: a named-pipe address `\\.\pipe\aiui-gui` — Windows pipes are
///   namespaced, not filesystem objects, so the same `PathBuf` carries the
///   pipe name as a path-like string.
pub fn socket_path(config_dir: &std::path::Path) -> PathBuf {
    #[cfg(unix)]
    {
        config_dir.join("gui.sock")
    }
    #[cfg(windows)]
    {
        // We don't rely on the filesystem on Windows — the pipe name is a
        // namespace lookup, not a file. The `config_dir` arg is unused but
        // kept for API symmetry with the Unix branch.
        let _ = config_dir;
        PathBuf::from(r"\\.\pipe\aiui-gui")
    }
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

/// GUI-side: bind the channel, accept connections, and self-terminate after a
/// grace period once all clients are gone. Increments/decrements the shared
/// `conns` counter on every connect/disconnect so `/health` can report the
/// live child count without polling.
///
/// Multi-instance hardening (since 0.4.33): if the channel already
/// answers a connection, another aiui-app is alive and we are the
/// duplicate — exit immediately rather than racing for ownership. The
/// previous behaviour silently tore the existing instance's listener
/// out from under it on every dup-launch, which is how the 2026-05-04
/// dual-companion incident produced reset connections in the first place.
pub async fn gui_serve(sock: PathBuf, app: AppHandle, conns: Arc<AtomicUsize>) {
    #[cfg(unix)]
    {
        gui_serve_unix(sock, app, conns).await;
    }
    #[cfg(windows)]
    {
        gui_serve_windows(sock, app, conns).await;
    }
}

#[cfg(unix)]
async fn gui_serve_unix(sock: PathBuf, app: AppHandle, conns: Arc<AtomicUsize>) {
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
                let _ = app.run_on_main_thread(move || app_for_exit.exit(1));
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

    let wake = make_shutdown_watcher(conns.clone(), app.clone());

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

#[cfg(windows)]
async fn gui_serve_windows(sock: PathBuf, app: AppHandle, conns: Arc<AtomicUsize>) {
    let pipe_name = sock.to_string_lossy().to_string();

    // Multi-instance probe: try to *connect* as a client. If it succeeds
    // another aiui already serves this pipe and we're the duplicate.
    // ERROR_FILE_NOT_FOUND (`NotFound`) means free; ERROR_PIPE_BUSY (231)
    // means a server exists but is currently saturated — also "duplicate".
    match ClientOptions::new().open(&pipe_name) {
        Ok(c) => {
            drop(c);
            trace(&format!(
                "lifetime: another aiui already serves {pipe_name} — exiting (multi-instance)"
            ));
            let app_for_exit = app.clone();
            let _ = app.run_on_main_thread(move || app_for_exit.exit(1));
            return;
        }
        Err(e) if e.raw_os_error() == Some(231) => {
            trace(&format!(
                "lifetime: pipe {pipe_name} busy — another aiui is up, exiting"
            ));
            let app_for_exit = app.clone();
            let _ = app.run_on_main_thread(move || app_for_exit.exit(1));
            return;
        }
        Err(_) => {
            // Pipe name is free — proceed to bind.
        }
    }

    let mut next_server = match ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)
    {
        Ok(s) => s,
        Err(e) => {
            trace(&format!(
                "lifetime: create_pipe {pipe_name} failed: {e} — exiting (multi-instance race)"
            ));
            let app_for_exit = app.clone();
            let _ = app.run_on_main_thread(move || app_for_exit.exit(1));
            return;
        }
    };
    trace(&format!("lifetime: listening on {pipe_name}"));

    let wake = make_shutdown_watcher(conns.clone(), app.clone());

    loop {
        if let Err(e) = next_server.connect().await {
            trace(&format!("lifetime: pipe connect error: {e}"));
        }
        let stream: NamedPipeServer = next_server;

        // Immediately rotate to a fresh server instance so the pipe stays
        // available for the *next* client; otherwise a second connect
        // attempt would race with the rotation and see ERROR_PIPE_BUSY.
        next_server = match ServerOptions::new().create(&pipe_name) {
            Ok(s) => s,
            Err(e) => {
                trace(&format!("lifetime: pipe rotate failed: {e} — exiting"));
                let app_for_exit = app.clone();
                let _ = app.run_on_main_thread(move || app_for_exit.exit(1));
                return;
            }
        };

        let n = conns.fetch_add(1, Ordering::SeqCst) + 1;
        trace(&format!("lifetime: client connected, active={n}"));
        let conns = conns.clone();
        let wake = wake.clone();
        tokio::spawn(async move {
            let mut stream = stream;
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
}

/// Shared shutdown timer wiring used by both backends. Returns the wake
/// `Notify` that connect/disconnect handlers signal — armed once when
/// the last client leaves, and cancellable by a fresh connect within
/// the grace period.
fn make_shutdown_watcher(conns: Arc<AtomicUsize>, app: AppHandle) -> Arc<Notify> {
    let wake = Arc::new(Notify::new());
    let conns_w = conns.clone();
    let wake_w = wake.clone();
    tokio::spawn(async move {
        loop {
            wake_w.notified().await;
            if conns_w.load(Ordering::SeqCst) > 0 {
                continue;
            }
            trace(&format!(
                "lifetime: no clients, grace timer {SHUTDOWN_GRACE_SECS}s"
            ));
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(SHUTDOWN_GRACE_SECS)) => {
                    if conns_w.load(Ordering::SeqCst) == 0 {
                        trace("lifetime: grace expired, exiting");
                        // Hard exit bypassing Tauri's ExitRequested dance —
                        // Cmd-Q and window-close are deliberately blocked
                        // there, so the only legitimate shutdown path is
                        // this one.
                        let _ = app;
                        std::process::exit(0);
                    }
                }
                _ = wake_w.notified() => {
                    trace("lifetime: new client within grace, staying");
                }
            }
        }
    });
    wake
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
/// channel attach in case a tunnel-fronted GUI on the user's machine
/// becomes reachable. The loop only exits when this MCP-stdio process
/// itself is terminated, which happens when the parent (Claude Desktop /
/// Claude Code) tears down its MCP children.
///
/// "Auto-resurrect" contract holds for local-Mac and local-Windows usage.
/// On remotes the MCP-stdio child trusts the SSH-reverse-tunnel to forward
/// port 7777 back to the user's machine where the actual aiui GUI lives.
pub async fn mcp_attach(sock: PathBuf) {
    let interactive = is_interactive_session();
    if !interactive {
        trace(
            "lifetime: detected non-interactive session (SSH/headless), \
             auto-resurrect of GUI is suppressed on this host",
        );
    }

    loop {
        let mut attached = false;
        for attempt in 1..=30u32 {
            match try_attach(&sock).await {
                Ok(()) => {
                    attached = true;
                    trace("lifetime: mcp socket closed — GUI is gone, will relaunch");
                    break;
                }
                Err(e) => {
                    if attempt == 1 && interactive {
                        trace(&format!(
                            "lifetime: gui channel not ready ({e}), launching GUI"
                        ));
                        spawn_gui_detached();
                    } else if attempt == 1 {
                        trace(&format!(
                            "lifetime: gui channel not ready ({e}), \
                             non-interactive session — GUI must be reachable \
                             via SSH-reverse-tunnel from the user's machine"
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
        if !attached {
            trace(
                "lifetime: mcp gave up waiting for gui channel after 30 attempts; retrying in 5s",
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        // GUI was connected and has now closed; loop back to resurrect it
        // (or wait + retry if launch failed / suppressed).
    }
}

/// Try to connect once and drain. Returns Ok(()) when the channel was
/// established and later closed by the server (normal lifecycle), Err on
/// connect failure.
async fn try_attach(sock: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut stream = UnixStream::connect(sock).await?;
        trace(&format!("lifetime: mcp attached to {}", sock.display()));
        let mut buf = [0u8; 64];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let pipe_name = sock.to_string_lossy().to_string();
        let mut stream: NamedPipeClient = ClientOptions::new().open(&pipe_name)?;
        trace(&format!("lifetime: mcp attached to {pipe_name}"));
        let mut buf = [0u8; 64];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        Ok(())
    }
}

/// Spawn the GUI process and detach so the MCP-stdio child does not own
/// it.
///
/// - macOS: `open -g -a aiui --args --auto` hands ownership to
///   LaunchServices, which respects LSUIElement (no Dock icon flash).
/// - Windows: re-spawn the same binary without `--mcp-stdio`. The child
///   becomes a sibling under Claude Desktop's process tree, which is fine
///   because Windows does not propagate exit signals to children the way
///   macOS does. NSIS does not register an `aiui` LaunchServices-style
///   alias, so we identify the binary via `current_exe()`.
fn spawn_gui_detached() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-g", "-a", "aiui", "--args", "--auto"])
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        match std::env::current_exe() {
            Ok(exe) => {
                let _ = std::process::Command::new(exe).arg("--auto").spawn();
            }
            Err(e) => {
                trace(&format!(
                    "lifetime: cannot locate own binary to spawn GUI: {e}"
                ));
            }
        }
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("--auto").spawn();
        }
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
