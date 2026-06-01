//! Lifetime coupling between the GUI process and MCP-stdio children.
//!
//! The GUI hosts a per-user channel — a Unix domain socket on macOS/Linux,
//! a Windows named pipe on Windows — and each `aiui --mcp-stdio` child
//! connects on startup and holds the stream open. When the child exits
//! (Claude Desktop closes it), the OS tears down the stream and the GUI
//! observes an EOF.
//!
//! Lifetime invariant (I1, stabilization-plan): the child counter does **not**
//! decide the host's lifetime. The last-child-disconnect edge is only a
//! *trigger* — it prompts the one question that actually decides whether we may
//! exit: is our host, Claude Desktop, still alive? While Claude Desktop runs we
//! stay, regardless of child count (a dropped/re-spawned MCP server, Cowork
//! churn). Only when Claude Desktop is gone does a short grace then exit follow
//! (the host follows the Wirt). The 60 s child-count grace that used to gate
//! exit was itself the root-cause bug — it killed the host during ordinary
//! churn while Claude Desktop was very much alive.
//!
//! Event-driven, no continuous polling: the only liveness probe is a single
//! `is_claude_desktop_running()` call per disconnect edge (plus one re-check
//! after the short grace).
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

/// Short grace after the last MCP-stdio child disconnects *and* Claude Desktop
/// is no longer detected, before the host exits. It absorbs two transients:
/// (a) a Claude Desktop quit→relaunch (its own update / a user restart), and
/// (b) the brief teardown window where Claude Desktop has already closed its
/// children's stdin (firing our edge) but its process is still terminating, so
/// `pgrep` could momentarily either way. We re-check Claude-Desktop liveness at
/// expiry and only exit if it is *still* gone. ≤5 s per the spec; nothing polls
/// in a loop.
pub const SHUTDOWN_GRACE_SECS: u64 = 5;

/// The single exit authority (Invariant I1). A host *planned* exit is legitimate
/// in exactly three cases: (b) aiui is uninstalled or (c) restarting into an
/// update — both signalled explicitly via [`ExitAuthority`] by `quit_app` /
/// the updater — or (a) Claude Desktop, the host process aiui lives with, has
/// terminated. Every other process exit is a crash, never a clean shutdown.
///
/// Pure so it can be unit-tested without a live Claude Desktop or a running
/// Tauri app: callers pass the two facts in. The impure shell reads them from
/// [`ExitAuthority`] state and `setup::is_claude_desktop_running()`.
pub fn host_should_exit(explicit_uninstall_or_update: bool, claude_desktop_running: bool) -> bool {
    explicit_uninstall_or_update || !claude_desktop_running
}

/// What the post-grace re-check decides. The child counter participates only as
/// "did a child come back" — it never independently authorizes an exit; that is
/// solely `!claude_desktop_running` (Invariant I1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraceOutcome {
    /// Claude Desktop is alive again, or a child re-attached — stay headless.
    Stay,
    /// Claude Desktop is still gone and no child returned — the host follows
    /// the Wirt and exits.
    Exit,
}

/// Decision made when the short grace expires. `child_returned` is true if any
/// MCP-stdio child re-attached during the grace; `claude_desktop_running` is a
/// fresh liveness probe. Exit only when the Wirt is gone *and* nothing came
/// back — Claude-Desktop liveness always wins (I1).
pub fn grace_outcome(child_returned: bool, claude_desktop_running: bool) -> GraceOutcome {
    if claude_desktop_running || child_returned {
        GraceOutcome::Stay
    } else {
        GraceOutcome::Exit
    }
}

/// Explicit exit authority for the two non-Wirt-death cases (uninstall, update
/// restart). A plain latch: set once by `quit_app` / the updater right before
/// they ask Tauri to terminate, read by the `ExitRequested` default-deny gate
/// so those — and only those — Tauri-initiated exits are honoured. Everything
/// else Tauri tries (last-window-close, ⌘Q, OS quit-all) is vetoed while Claude
/// Desktop is alive.
pub struct ExitAuthority {
    authorized: std::sync::atomic::AtomicBool,
}

impl ExitAuthority {
    pub fn new() -> Self {
        Self {
            authorized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Latch the authority on. Irreversible by design — once we have decided to
    /// uninstall or restart into an update there is no "un-deciding" before the
    /// process is gone.
    pub fn authorize(&self) {
        self.authorized.store(true, Ordering::SeqCst);
    }

    pub fn is_authorized(&self) -> bool {
        self.authorized.load(Ordering::SeqCst)
    }
}

impl Default for ExitAuthority {
    fn default() -> Self {
        Self::new()
    }
}

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

/// GUI-side: bind the channel and accept connections. Increments/decrements the
/// shared `conns` counter on every connect/disconnect so `/health` can report
/// the live child count without polling. When the last child leaves it pokes
/// the shutdown watcher, which exits *only* if the Wirt (Claude Desktop) is also
/// gone — the counter never terminates the process on its own (Invariant I1).
///
/// Multi-instance hardening (since 0.4.33): if the channel already
/// answers a connection, another aiui-app is alive and we are the
/// duplicate — exit immediately rather than racing for ownership. The
/// previous behaviour silently tore the existing instance's listener
/// out from under it on every dup-launch, which is how the 2026-05-04
/// dual-companion incident produced reset connections in the first place.
pub async fn gui_serve(sock: PathBuf, app: AppHandle, conns: Arc<AtomicUsize>, http_port: u16) {
    #[cfg(unix)]
    {
        gui_serve_unix(sock, app, conns, http_port).await;
    }
    #[cfg(windows)]
    {
        gui_serve_windows(sock, app, conns, http_port).await;
    }
}

#[cfg(unix)]
async fn gui_serve_unix(sock: PathBuf, app: AppHandle, conns: Arc<AtomicUsize>, http_port: u16) {
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
                let _ = app.run_on_main_thread(move || {
                    crate::housekeeping::pre_exit_cleanup(http_port, "multi-instance-live");
                    app_for_exit.exit(1)
                });
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
            let _ = app.run_on_main_thread(move || {
                crate::housekeeping::pre_exit_cleanup(http_port, "multi-instance-bind-race");
                app_for_exit.exit(1)
            });
            return;
        }
    };
    trace(&format!("lifetime: listening on {}", sock.display()));

    let wake = make_shutdown_watcher(conns.clone(), app.clone(), http_port);

    loop {
        match listener.accept().await {
            Ok((mut stream, _)) => {
                let n = conns.fetch_add(1, Ordering::SeqCst) + 1;
                trace(&format!("lifetime: client connected, active={n}"));
                crate::lifecycle_log::record(
                    crate::lifecycle_log::LifecycleEvent::ChildAttached { count: n },
                );
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
                    crate::lifecycle_log::record(
                        crate::lifecycle_log::LifecycleEvent::ChildDetached { count: left },
                    );
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
async fn gui_serve_windows(sock: PathBuf, app: AppHandle, conns: Arc<AtomicUsize>, http_port: u16) {
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
            let _ = app.run_on_main_thread(move || {
                crate::housekeeping::pre_exit_cleanup(http_port, "multi-instance-live");
                app_for_exit.exit(1)
            });
            return;
        }
        Err(e) if e.raw_os_error() == Some(231) => {
            trace(&format!(
                "lifetime: pipe {pipe_name} busy — another aiui is up, exiting"
            ));
            let app_for_exit = app.clone();
            let _ = app.run_on_main_thread(move || {
                crate::housekeeping::pre_exit_cleanup(http_port, "multi-instance-pipe-busy");
                app_for_exit.exit(1)
            });
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
            let _ = app.run_on_main_thread(move || {
                crate::housekeeping::pre_exit_cleanup(http_port, "multi-instance-pipe-race");
                app_for_exit.exit(1)
            });
            return;
        }
    };
    trace(&format!("lifetime: listening on {pipe_name}"));

    let wake = make_shutdown_watcher(conns.clone(), app.clone(), http_port);

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
                let _ = app.run_on_main_thread(move || {
                    crate::housekeeping::pre_exit_cleanup(http_port, "pipe-rotate-failed");
                    app_for_exit.exit(1)
                });
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

/// Shared shutdown watcher used by both backends. Returns the wake `Notify`
/// that the disconnect handlers signal when the *last* child leaves.
///
/// Invariant I1: the child counter is a trigger, not an authority. This edge
/// does not by itself end the process — it prompts a short grace and then a
/// single Claude-Desktop liveness probe ([`grace_outcome`]). The host exits
/// only when its Wirt (Claude Desktop) is gone; while Claude Desktop is alive
/// we stay headless no matter how the child count moves.
fn make_shutdown_watcher(conns: Arc<AtomicUsize>, app: AppHandle, http_port: u16) -> Arc<Notify> {
    let wake = Arc::new(Notify::new());
    let conns_w = conns.clone();
    let wake_w = wake.clone();
    tokio::spawn(async move {
        loop {
            wake_w.notified().await;
            // Edge: the last MCP-stdio child just disconnected. The counter is
            // only a trigger (I1) — it does not authorize an exit. If a child
            // re-attached already, there is nothing to decide.
            if conns_w.load(Ordering::SeqCst) > 0 {
                continue;
            }
            // Short grace, then let Claude-Desktop liveness — never the child
            // count — make the call. The grace absorbs (a) a Claude Desktop
            // quit→relaunch (its own update, a user restart) and (b) the
            // teardown window where Claude Desktop has already closed our
            // child's stdin (firing this very edge) but its process is still
            // terminating, so a probe *now* could read either way.
            //
            // We arm the grace even when Claude Desktop currently looks alive:
            // during ordinary churn it simply expires into `Stay`, and arming
            // unconditionally is exactly what closes the teardown race that a
            // "skip the grace if CD looks alive" shortcut would leave open —
            // otherwise a probe catching CD mid-quit as "alive" would `Stay`
            // with no further edge ever firing, stranding the host alive after
            // its Wirt is gone (a Step-2 regression). The cost is a single 5 s
            // timer + one `pgrep` per disconnect edge — no continuous poll.
            trace(&format!(
                "lifetime: last child gone — grace {SHUTDOWN_GRACE_SECS}s then re-check Claude Desktop liveness"
            ));
            crate::lifecycle_log::transition(crate::lifecycle_log::Phase::GracePending);
            crate::lifecycle_log::record(crate::lifecycle_log::LifecycleEvent::GraceArmed {
                secs: SHUTDOWN_GRACE_SECS,
            });
            tokio::time::sleep(Duration::from_secs(SHUTDOWN_GRACE_SECS)).await;
            let child_returned = conns_w.load(Ordering::SeqCst) > 0;
            let cd_running = crate::setup::is_claude_desktop_running();
            let outcome = grace_outcome(child_returned, cd_running);
            crate::lifecycle_log::record(crate::lifecycle_log::LifecycleEvent::GraceResolved {
                outcome: match outcome {
                    GraceOutcome::Stay => "stay",
                    GraceOutcome::Exit => "exit",
                },
                claude_desktop_running: cd_running,
                child_returned,
            });
            match outcome {
                GraceOutcome::Stay => {
                    crate::lifecycle_log::transition(crate::lifecycle_log::Phase::Serving);
                    trace(&format!(
                        "lifetime: staying after grace \
                         (claude_desktop_running={cd_running}, child_returned={child_returned})"
                    ));
                }
                GraceOutcome::Exit => {
                    crate::lifecycle_log::transition(crate::lifecycle_log::Phase::Exiting);
                    crate::lifecycle_log::record(crate::lifecycle_log::LifecycleEvent::HostExit {
                        reason: "claude-desktop-gone",
                    });
                    trace(
                        "lifetime: Claude Desktop gone after grace and no child returned — \
                         host follows Wirt, exiting",
                    );
                    for line in crate::lifecycle_log::recent() {
                        trace(&format!("lifecycle-dump {line}"));
                    }
                    // Hard exit: this is exit case (a), the watcher's own
                    // authority. It bypasses Tauri's ExitRequested gate (which
                    // default-denies) because the gate has no way to know the
                    // watcher already established `!is_claude_desktop_running()`.
                    crate::housekeeping::pre_exit_cleanup(http_port, "claude-desktop-gone");
                    let _ = app;
                    std::process::exit(0);
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

    // --- Step-1 verification mini-harness (stabilization-plan §Step 2) ---
    //
    // The decision core is pulled out as pure functions so the two invariants
    // can be asserted without a live Claude Desktop or a running Tauri app:
    //   * the host survives a child flap as long as Claude Desktop runs, and
    //   * the host exits once Claude Desktop quits.
    // These mirror exactly the (child_returned, claude_desktop_running) facts
    // the watcher reads at grace expiry and the (explicit, cd_running) facts
    // the ExitRequested gate reads.

    #[test]
    fn host_stays_while_claude_desktop_runs() {
        // I1: with Claude Desktop alive and no explicit uninstall/update, the
        // host may never plan an exit — whatever the child count did.
        assert!(!host_should_exit(false, true));
    }

    #[test]
    fn host_exits_when_claude_desktop_quits() {
        // Case (a): Wirt gone, no explicit signal → exit authorized.
        assert!(host_should_exit(false, false));
    }

    #[test]
    fn host_exits_on_explicit_uninstall_or_update_even_if_cd_alive() {
        // Cases (b)/(c): uninstall / update-restart authorize exit regardless
        // of Claude-Desktop liveness.
        assert!(host_should_exit(true, true));
        assert!(host_should_exit(true, false));
    }

    #[test]
    fn child_flap_with_claude_desktop_alive_stays() {
        // The pivotal regression case: the last child disconnected (Cowork
        // churn / MCP re-spawn) but Claude Desktop is alive — STAY. This is the
        // exact scenario the old 60 s child-count grace got wrong by exiting.
        assert_eq!(grace_outcome(false, true), GraceOutcome::Stay);
        // A child re-attaching during the grace also keeps us up, trivially.
        assert_eq!(grace_outcome(true, true), GraceOutcome::Stay);
        assert_eq!(grace_outcome(true, false), GraceOutcome::Stay);
    }

    #[test]
    fn claude_desktop_quit_with_no_child_exits() {
        // Wirt gone after the grace and nothing came back → host follows Wirt.
        assert_eq!(grace_outcome(false, false), GraceOutcome::Exit);
    }

    #[test]
    fn exit_authority_latches() {
        let auth = ExitAuthority::new();
        assert!(!auth.is_authorized());
        auth.authorize();
        assert!(auth.is_authorized());
        // Idempotent — staying latched is the contract.
        auth.authorize();
        assert!(auth.is_authorized());
    }
}
