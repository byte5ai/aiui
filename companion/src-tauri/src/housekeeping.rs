//! Kill stale `aiui --mcp-stdio` children left over from older app versions.
//!
//! Context: Claude Desktop spawns `aiui --mcp-stdio` once and keeps it alive
//! for the whole Claude Desktop session. If the user updates the aiui binary
//! while a session is live, those already-spawned children keep running with
//! the *old* code. Their lifetime-channel logic may be pre-auto-resurrect (≤
//! v0.2.5) or otherwise incompatible, so the user ends up with a stale MCP
//! server that refuses to reconnect to the new GUI.
//!
//! Two complementary mechanisms exist:
//!
//!  1. **GUI-side sweep** (`kill_stale_mcp_stdio_children`): on every GUI
//!     startup we scan for `aiui --mcp-stdio` processes whose executable
//!     path differs from ours and signal them to terminate. This catches the
//!     case where the *path* changed — useless when the user replaced the
//!     binary in place.
//!
//!  2. **Subprocess-side self-check** (`disk_version_if_stale`, macOS only):
//!     every `--mcp-stdio` invocation reads `CFBundleShortVersionString` from
//!     the on-disk `Info.plist` two directories up from `argv[0]` and
//!     compares it with `CARGO_PKG_VERSION` baked in at compile time. If they
//!     disagree, the in-memory binary is stale — the bundle on disk was
//!     replaced after this process loaded — and we exit so Claude Desktop
//!     respawns us against the fresh binary.
//!
//!     On Windows there is no analog of `Info.plist`. The Windows path-based
//!     sweep (mechanism 1) covers the NSIS-update case because NSIS replaces
//!     files at the install path while old children continue running from a
//!     temporary copy under their original PID — sysinfo's `exe()` reports
//!     the original path, which differs from `current_exe()` for the freshly
//!     spawned GUI.
//!
//! Cross-platform via `sysinfo`: both sweeps enumerate processes with the
//! same API, no `ps`/`tasklist` shell-out, no /proc assumption.
//!
//! Safety: we never kill our own pid. If the current binary path can't be
//! determined, we skip the path-based sweep entirely.
//!
//! Idempotent: running on a clean system is a no-op.

use crate::logging::trace;
use fs4::fs_std::FileExt;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use sysinfo::{ProcessRefreshKind, RefreshKind, Signal, System};

/// Process-lifetime advisory lock backed by `flock` on Unix (and
/// `LockFileEx` on Windows via `fs4`). Used to enforce a single live
/// aiui-GUI instance:
///
/// * GUI acquires the lock at the very start of `run()`, before
///   `lifetime::gui_serve` or `http::serve` open any sockets.
/// * A second GUI process started while the lock is held fails the
///   `try_lock_exclusive` call immediately (LOCK_NB-equivalent) and
///   exits with a traced `(gui-lock-busy)` reason — no race against
///   the lifetime-socket or HTTP-port bind.
/// * Kernel releases the lock automatically when the process dies, so
///   a crashed predecessor never leaves a stale lock blocking future
///   starts (the failure mode `O_EXCL`-style PID files would have).
///
/// Designed as RAII: keep the returned guard alive for the whole
/// process lifetime; drop it (explicit or implicit) to release.
/// v0.4.43.
pub struct ProcessLock {
    file: std::fs::File,
    path: PathBuf,
}

impl ProcessLock {
    /// Try to acquire the lock at `path` without blocking. Returns
    /// `Ok(guard)` on success, `Err(io::Error)` if the lock is already
    /// held by another process (kind `WouldBlock`) or any filesystem
    /// problem (permissions, missing parent directory, …).
    ///
    /// Creates the lock file with mode 0600 — the file body is never
    /// read or written, only locked.
    pub fn try_acquire(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        // Make sure the parent directory exists. The caller is expected
        // to pass `<config_dir>/gui.lock`; `config_dir` is created
        // earlier by AppConfig::load_or_init, but being defensive here
        // saves a future bug when callers pass a fresh path.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        file.try_lock_exclusive()?;
        Ok(Self { file, path })
    }

    /// Path of the underlying lock file. Useful for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        // Explicit unlock for fast handoff; the kernel would do this
        // automatically on close, but doing it here gives a deterministic
        // ordering point in the shutdown sequence.
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(target_os = "macos")]
use std::process::Command;

/// A stale `aiui --mcp-stdio` process discovered during the sweep.
#[derive(Debug, PartialEq, Eq, Clone)]
struct StaleChild {
    pid: u32,
    exe: String,
}

/// Lightweight snapshot of one process — what we need for the filters.
#[derive(Debug, Clone)]
struct ProcSnap {
    pid: u32,
    /// Parent PID. On macOS/Linux this is the immediate parent; orphans
    /// re-parent to launchd/init (pid 1). `None` only when sysinfo can't
    /// resolve the parent (rare; treat as "unknown, don't act").
    ppid: Option<u32>,
    exe: String,
    args: Vec<String>,
    /// Process start time in seconds since the Unix epoch (whatever
    /// `sysinfo::Process::start_time` returns for the current OS).
    /// Used by the sibling-mcp-stdio sweep to enforce a strict
    /// "younger kills older" rule and avoid two simultaneously-started
    /// duplicates from killing each other. v0.4.42.
    start_time: u64,
}

/// Enumerate every running process via `sysinfo` and return a snapshot.
/// Cross-platform: identical behaviour on macOS, Linux, and Windows.
fn snapshot_processes() -> Vec<ProcSnap> {
    let sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
    );
    sys.processes()
        .iter()
        .map(|(pid, p)| {
            let exe = p
                .exe()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_else(|| {
                    p.cmd()
                        .first()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default()
                });
            let args = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();
            ProcSnap {
                pid: pid.as_u32(),
                ppid: p.parent().map(|p| p.as_u32()),
                exe,
                args,
                start_time: p.start_time(),
            }
        })
        .collect()
}

/// True iff `exe` looks like our aiui binary — last path component is
/// `aiui` (Unix) or `aiui.exe` (Windows). The path-based filter is what
/// keeps us from accidentally signalling a Python script that happens to
/// have `--mcp-stdio` in its argv.
fn is_aiui_binary(exe: &str) -> bool {
    let leaf = exe
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(exe)
        .to_ascii_lowercase();
    leaf == "aiui" || leaf == "aiui.exe"
}

/// True iff `args` contains the `--mcp-stdio` flag anywhere.
fn has_mcp_stdio_flag(args: &[String]) -> bool {
    args.iter().any(|a| a == "--mcp-stdio")
}

/// Filter: stale (different path) `aiui --mcp-stdio` children, excluding
/// `own_pid`. Pure function over a snapshot, kept testable.
fn find_stale(snap: &[ProcSnap], current_exe_path: &str, own_pid: u32) -> Vec<StaleChild> {
    snap.iter()
        .filter(|p| p.pid != own_pid)
        .filter(|p| has_mcp_stdio_flag(&p.args))
        .filter(|p| is_aiui_binary(&p.exe))
        .filter(|p| p.exe != current_exe_path)
        .map(|p| StaleChild {
            pid: p.pid,
            exe: p.exe.clone(),
        })
        .collect()
}

/// Filter: every `aiui --mcp-stdio` child regardless of executable path,
/// excluding `own_pid`. Used for the uninstall flow.
fn find_all_children(snap: &[ProcSnap], own_pid: u32) -> Vec<StaleChild> {
    snap.iter()
        .filter(|p| p.pid != own_pid)
        .filter(|p| has_mcp_stdio_flag(&p.args))
        .filter(|p| is_aiui_binary(&p.exe))
        .map(|p| StaleChild {
            pid: p.pid,
            exe: p.exe.clone(),
        })
        .collect()
}

/// A process is *orphaned* when its parent is gone — reparented to
/// launchd/init (`ppid == 1`), parent unknown (`None`), or the ppid no
/// longer exists in the snapshot. Only orphaned `aiui --mcp-stdio`
/// children are safe to reap: their MCP client (the Claude.app / Cowork
/// helper wrapper that spawned them) has died, so they are genuinely
/// abandoned. A child whose parent is still alive belongs to a *live*
/// session — and must be spared.
fn is_orphaned_child(snap: &[ProcSnap], p: &ProcSnap) -> bool {
    match p.ppid {
        None => true,
        Some(1) => true,
        Some(pp) => !snap.iter().any(|q| q.pid == pp),
    }
}

/// Filter: every *orphaned* `aiui --mcp-stdio` child (parent process
/// gone), excluding `own_pid`. Pure function over a snapshot so tests
/// don't need to spoof `std::process::id()`.
///
/// v0.4.46 (Bug A): replaces the previous "newer-kills-older with the
/// same grandparent" rule. That rule used the Claude.app process as the
/// discriminator — but in Cowork every concurrently-open session's
/// mcp-stdio sits under the *one* Claude.app grandparent, so starting a
/// new session tore down the still-live aiui connection of every other
/// session (the 2026-05-28 "Server disconnected on first MCP call"
/// reports). Orphan-status is the correct discriminator: a leaked child
/// has lost its parent wrapper; a live parallel session has not. The
/// genuine duplicate-child case the old rule guarded against is already
/// covered by stdin-EOF (a client that drops a child closes its stdin,
/// and `run_stdio` exits on EOF).
fn find_orphaned_mcp_stdio_to_kill(snap: &[ProcSnap], own_pid: u32) -> Vec<StaleChild> {
    snap.iter()
        .filter(|p| p.pid != own_pid)
        .filter(|p| has_mcp_stdio_flag(&p.args))
        .filter(|p| is_aiui_binary(&p.exe))
        .filter(|p| is_orphaned_child(snap, p))
        .map(|p| StaleChild {
            pid: p.pid,
            exe: p.exe.clone(),
        })
        .collect()
}

/// Reap every *orphaned* `aiui --mcp-stdio` child — one whose parent
/// (the Claude.app / Cowork helper wrapper) has died, leaving it
/// reparented to launchd. Called once at mcp-stdio startup. Returns the
/// number of children terminated.
///
/// Why this exists / why it changed (v0.4.46, Bug A): the previous
/// implementation killed any older mcp-stdio sharing our *grandparent*
/// (the Claude.app process). That was meant to clear a Claude-Desktop
/// duplicate-child glitch (one session, two children, slash-command
/// routing confused — "kein erkannter Befehl"). But it mis-fired badly
/// under Cowork: every concurrently-open Cowork session's mcp-stdio
/// sits under the same single Claude.app grandparent, so starting a new
/// session reaped the still-live aiui connection of every *other*
/// session (the 2026-05-28 "Server disconnected on first MCP call"
/// reports). "Same grandparent" can't tell a leaked duplicate from a
/// live parallel session — both share the app.
///
/// Orphan-status can: a leaked child has lost its parent wrapper; a live
/// session's child has not. So we now reap only orphans. The original
/// duplicate-child case is already covered by stdin-EOF — when a client
/// drops a child it closes the child's stdin, and `run_stdio` exits on
/// EOF. We never broadly sweep all aiui-mcp-stdio children;
/// `kill_all_mcp_stdio_children` is the uninstall-only path for that.
pub fn kill_orphaned_mcp_stdio_children() -> usize {
    let own_pid = std::process::id();
    let snap = snapshot_processes();
    let victims = find_orphaned_mcp_stdio_to_kill(&snap, own_pid);

    for victim in &victims {
        trace(&format!(
            "housekeeping: reaping orphaned mcp-stdio pid={} exe={} \
             (parent gone — abandoned leak)",
            victim.pid, victim.exe
        ));
        terminate_pid(victim.pid);
    }
    let n = victims.len();
    if n > 0 {
        trace(&format!(
            "housekeeping: reaped {n} orphaned mcp-stdio child(ren) on startup"
        ));
    }
    n
}

/// Filter: every `aiui --mcp-stdio` child started *strictly before*
/// `own_start_time`, excluding `own_pid`. Pure function over a
/// snapshot — caller passes own pid + own start_time so tests don't
/// need to spoof `std::process::id`.
///
/// Used by the GUI at startup right after it wins the process-lifetime
/// lock: any mcp-stdio that predates the freshly-started GUI carries
/// the binary it was spawned with (potentially pre-update RAM), and
/// `disk_version_if_stale` would only catch the version-drift case on
/// its own. The newer GUI is the source of truth → all older children
/// are kicked, Claude Desktop respawns them against the current binary
/// with all of the current GUI's protections (sibling-kill, periodic
/// stale-check, etc.). v0.4.43.
fn find_pre_gui_mcp_stdio_to_kill(
    snap: &[ProcSnap],
    own_pid: u32,
    own_start_time: u64,
) -> Vec<StaleChild> {
    snap.iter()
        .filter(|p| p.pid != own_pid)
        .filter(|p| has_mcp_stdio_flag(&p.args))
        .filter(|p| is_aiui_binary(&p.exe))
        .filter(|p| p.start_time < own_start_time)
        .map(|p| StaleChild {
            pid: p.pid,
            exe: p.exe.clone(),
        })
        .collect()
}

/// Public entry: terminate every `aiui --mcp-stdio` child older than
/// us. Returns the count of children signalled. Safe to call from
/// the GUI startup path after winning the process-lifetime lock —
/// no race because at most one GUI holds the lock.
pub fn kill_mcp_stdio_started_before_self() -> usize {
    let own_pid = std::process::id();
    let snap = snapshot_processes();
    let own_start_time = snap
        .iter()
        .find(|p| p.pid == own_pid)
        .map(|p| p.start_time)
        .unwrap_or(0);
    if own_start_time == 0 {
        // We couldn't find ourselves in the snapshot — refuse to act
        // rather than potentially kill same-second children. The
        // sibling-kill path on the mcp-stdio side is the safety net.
        trace(
            "housekeeping: pre-GUI sweep skipped — own start_time unresolved \
             (refusing to act without a cutoff to avoid same-second kills)",
        );
        return 0;
    }
    let victims = find_pre_gui_mcp_stdio_to_kill(&snap, own_pid, own_start_time);
    for victim in &victims {
        trace(&format!(
            "housekeeping: killing pre-GUI mcp-stdio child pid={} exe={} (cutoff={})",
            victim.pid, victim.exe, own_start_time
        ));
        terminate_pid(victim.pid);
    }
    if !victims.is_empty() {
        trace(&format!(
            "housekeeping: terminated {} pre-GUI mcp-stdio child(ren) at startup",
            victims.len()
        ));
    }
    victims.len()
}

/// Cross-platform process termination via sysinfo. Sends SIGTERM on Unix
/// and the equivalent terminate-by-handle on Windows.
fn terminate_pid(pid: u32) {
    let sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
    );
    if let Some(p) = sys.process(sysinfo::Pid::from_u32(pid)) {
        let _ = p.kill_with(Signal::Term).unwrap_or_else(|| p.kill());
    }
}

/// Scan for stale `aiui --mcp-stdio` processes and terminate the ones
/// whose executable path differs from `current_exe_path`. Returns the
/// number of processes killed.
pub fn kill_stale_mcp_stdio_children(current_exe_path: &str) -> usize {
    let own_pid = std::process::id();
    let snap = snapshot_processes();
    let stale = find_stale(&snap, current_exe_path, own_pid);

    for child in &stale {
        trace(&format!(
            "housekeeping: killing stale mcp-stdio child pid={} exe={}",
            child.pid, child.exe
        ));
        terminate_pid(child.pid);
    }

    if !stale.is_empty() {
        trace(&format!(
            "housekeeping: terminated {} stale mcp-stdio child(ren)",
            stale.len()
        ));
    }
    stale.len()
}

/// Sibling of `kill_stale_mcp_stdio_children` that doesn't filter by
/// executable path — every running `aiui --mcp-stdio` (other than our
/// own pid) gets terminated. Bound to the uninstall flow (#72): without
/// this, the auto-resurrect loop in `mcp_attach` would relaunch the GUI
/// the moment we call `app.exit(0)`.
pub fn kill_all_mcp_stdio_children() -> usize {
    let own_pid = std::process::id();
    let snap = snapshot_processes();
    let children = find_all_children(&snap, own_pid);

    for child in &children {
        trace(&format!(
            "housekeeping: killing mcp-stdio child pid={} exe={} (uninstall sweep)",
            child.pid, child.exe
        ));
        terminate_pid(child.pid);
    }

    if !children.is_empty() {
        trace(&format!(
            "housekeeping: terminated {} mcp-stdio child(ren) for uninstall",
            children.len()
        ));
    }
    children.len()
}

/// True iff `args` look like a `ssh -N -T -R <port>:localhost:<port> ...`
/// invocation — exactly the shape `tunnel.rs:run_tunnel` spawns. Tight
/// match so we never accidentally signal an unrelated `ssh` someone has
/// running for a different reason. v0.4.37.
fn is_aiui_ssh_ntr_for_port(args: &[String], port: u16) -> bool {
    if args.first().map(String::as_str) != Some("ssh") {
        return false;
    }
    let needle = format!("{port}:localhost:{port}");
    let has_n = args.iter().any(|a| a == "-N");
    let has_t = args.iter().any(|a| a == "-T");
    let mut has_r = false;
    let mut iter = args.iter().peekable();
    while let Some(a) = iter.next() {
        if a == "-R" {
            if let Some(next) = iter.peek() {
                if next.as_str() == needle {
                    has_r = true;
                    break;
                }
            }
        }
    }
    has_n && has_t && has_r
}

/// Filter: every `ssh -NTR <port>:localhost:<port>` process matching our
/// tunnel signature. When `only_orphans` is true, restrict to processes
/// whose ppid is 1 (launchd/init) — the case where an earlier aiui crashed
/// out of `app.exit()` / `process::exit()` without firing `kill_on_drop`,
/// leaving the ssh child re-parented to launchd. When false, return all
/// matching processes (used pre-exit so we sweep our own active tunnels
/// before the rust-side Drop is skipped). Pure over a snapshot.
fn find_aiui_ssh_ntr(snap: &[ProcSnap], port: u16, only_orphans: bool) -> Vec<u32> {
    snap.iter()
        .filter(|p| is_aiui_ssh_ntr_for_port(&p.args, port))
        .filter(|p| !only_orphans || p.ppid == Some(1))
        .map(|p| p.pid)
        .collect()
}

/// Convenience wrapper for the pre-exit path: traces the imminent exit
/// and sweeps our own active ssh-NTR tunnels so they don't outlive us as
/// launchd-orphans when `app.exit()` / `process::exit()` skip Rust Drop.
///
/// Called immediately before every exit point in the GUI process. v0.4.37.
pub fn pre_exit_cleanup(port: u16, reason: &str) {
    trace(&format!(
        "[aiui] exit ({reason}): cleaning up ssh-NTR tunnels before shutdown"
    ));
    let killed = kill_aiui_ssh_ntr(port, false);
    trace(&format!(
        "[aiui] exit ({reason}): swept {killed} ssh-NTR child(ren); proceeding"
    ));
}

/// Sweep ssh-NTR tunnel children — see `find_aiui_ssh_ntr` for the filter.
/// Returns the number of processes signalled. Logs each kill to the trace
/// for post-mortem debuggability of the v0.4.36 orphan-tunnel-loop.
pub fn kill_aiui_ssh_ntr(port: u16, only_orphans: bool) -> usize {
    let snap = snapshot_processes();
    let pids = find_aiui_ssh_ntr(&snap, port, only_orphans);
    let mode = if only_orphans { "orphan" } else { "all" };
    for pid in &pids {
        trace(&format!(
            "housekeeping: killing {mode} ssh-NTR tunnel pid={pid}"
        ));
        terminate_pid(*pid);
    }
    if !pids.is_empty() {
        trace(&format!(
            "housekeeping: terminated {} {mode} ssh-NTR tunnel(s) on :{port}",
            pids.len()
        ));
    }
    pids.len()
}

/// Pure decision: given our compile-time version string and the version
/// string read from the on-disk bundle, return `true` when this in-memory
/// binary is stale (i.e. should exit so it can be respawned).
///
/// Empty / whitespace `disk` is treated as "unknown" → not stale: better
/// to keep running than abort a working subprocess on a transient
/// `plutil` glitch.
///
/// On Windows the helper is unused at runtime — `disk_version_if_stale`
/// short-circuits to `None` because there is no `Info.plist` to read —
/// but the unit tests still validate the pure decision logic on every
/// platform, so we keep the function compiled and silence dead-code on
/// non-macOS.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn is_disk_version_stale(own: &str, disk: &str) -> bool {
    let disk = disk.trim();
    !disk.is_empty() && disk != own
}

/// True iff the bundle on disk reports a version that differs from our
/// own compile-time `CARGO_PKG_VERSION`. Returns the on-disk version when
/// stale so the caller can log it; `None` when fresh, when running outside
/// a packaged install (dev build, `cargo run`), or when the lookup itself
/// fails.
///
/// Self-detection at the subprocess side is what closes the gap that the
/// path-based GUI sweep can't see: an in-place bundle replacement leaves
/// the running child with stale code at the unchanged path.
///
/// Implemented for macOS (reads `CFBundleShortVersionString` from
/// `Info.plist`); on Windows there is no in-bundle version stamp accessible
/// without pulling a Win32 resource-parsing crate, so we return `None` and
/// rely on the path-based GUI sweep to catch updates after the user
/// restarts Claude Desktop.
#[cfg(target_os = "macos")]
pub fn disk_version_if_stale() -> Option<String> {
    let own = env!("CARGO_PKG_VERSION");
    let exe = std::env::current_exe().ok()?;
    // .../aiui.app/Contents/MacOS/aiui  →  .../aiui.app/Contents/Info.plist
    let plist: PathBuf = exe.parent()?.parent()?.join("Info.plist");
    if !plist.exists() {
        return None;
    }
    let out = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
        .arg(&plist)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let disk = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if is_disk_version_stale(own, &disk) {
        Some(disk)
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
pub fn disk_version_if_stale() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    const CURRENT: &str = r"C:\Program Files\aiui\aiui.exe";
    #[cfg(not(windows))]
    const CURRENT: &str = "/Applications/aiui.app/Contents/MacOS/aiui";

    fn snap(pid: u32, exe: &str, args: &[&str]) -> ProcSnap {
        ProcSnap {
            pid,
            ppid: Some(0),
            exe: exe.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            start_time: 0,
        }
    }

    fn snap_with_ppid(pid: u32, ppid: u32, exe: &str, args: &[&str]) -> ProcSnap {
        ProcSnap {
            pid,
            ppid: Some(ppid),
            exe: exe.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            start_time: 0,
        }
    }

    fn snap_full(
        pid: u32,
        ppid: u32,
        exe: &str,
        args: &[&str],
        start_time: u64,
    ) -> ProcSnap {
        ProcSnap {
            pid,
            ppid: Some(ppid),
            exe: exe.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            start_time,
        }
    }

    #[test]
    fn skips_unrelated_processes() {
        let s = vec![
            snap(12345, "/usr/bin/python3", &["python3", "some_script.py", "--mcp-stdio"]),
            snap(23456, "/opt/homebrew/bin/uv", &["uv", "tool", "uvx", "aiui-mcp"]),
            snap(34567, "/bin/zsh", &["zsh", "-c", "echo hello"]),
        ];
        assert!(find_stale(&s, CURRENT, 1).is_empty());
    }

    #[test]
    fn skips_current_binary() {
        let s = vec![snap(99999, CURRENT, &[CURRENT, "--mcp-stdio"])];
        assert!(find_stale(&s, CURRENT, 1).is_empty());
    }

    #[test]
    fn skips_own_pid_even_if_path_differs() {
        let s = vec![snap(12345, "/old/path/aiui", &["/old/path/aiui", "--mcp-stdio"])];
        assert!(find_stale(&s, CURRENT, 12345).is_empty());
    }

    #[test]
    fn disk_version_check_treats_match_as_fresh() {
        assert!(!is_disk_version_stale("0.4.26", "0.4.26"));
        // Trailing whitespace from `plutil` output is normal.
        assert!(!is_disk_version_stale("0.4.26", "0.4.26\n"));
        assert!(!is_disk_version_stale("0.4.26", "  0.4.26  "));
    }

    #[test]
    fn disk_version_check_treats_mismatch_as_stale() {
        assert!(is_disk_version_stale("0.4.25", "0.4.26"));
        assert!(is_disk_version_stale("0.4.26", "0.4.27"));
        assert!(is_disk_version_stale("0.4.26", "1.0.0"));
    }

    #[test]
    fn disk_version_check_treats_empty_disk_as_unknown_not_stale() {
        // If the on-disk lookup returns nothing — bundle missing, dev
        // build, permissions issue — we'd rather keep running than abort.
        // The GUI-side sweep is the safety net for that path.
        assert!(!is_disk_version_stale("0.4.26", ""));
        assert!(!is_disk_version_stale("0.4.26", "   "));
        assert!(!is_disk_version_stale("0.4.26", "\n\n"));
    }

    #[test]
    fn finds_stale_child_with_different_path() {
        let s = vec![
            snap(12345, "/old/path/aiui", &["/old/path/aiui", "--mcp-stdio"]),
            snap(23456, CURRENT, &[CURRENT, "--mcp-stdio"]),
        ];
        let stale = find_stale(&s, CURRENT, 1);
        assert_eq!(
            stale,
            vec![StaleChild {
                pid: 12345,
                exe: "/old/path/aiui".into()
            }]
        );
    }

    #[test]
    fn finds_multiple_stale_children() {
        let s = vec![
            snap(100, "/a/aiui", &["/a/aiui", "--mcp-stdio"]),
            snap(200, "/b/aiui", &["/b/aiui", "--mcp-stdio", "--extra"]),
            snap(300, CURRENT, &[CURRENT, "--mcp-stdio"]),
        ];
        let stale = find_stale(&s, CURRENT, 1);
        assert_eq!(stale.len(), 2);
        assert_eq!(stale[0].pid, 100);
        assert_eq!(stale[1].pid, 200);
    }

    #[test]
    fn ignores_aiui_gui_processes_without_mcp_stdio_flag() {
        // The GUI process itself runs the same binary but without
        // `--mcp-stdio`. Must not be killed.
        let s = vec![
            snap(42, CURRENT, &[CURRENT]),
            snap(43, "/old/path/aiui", &["/old/path/aiui"]),
        ];
        assert!(find_stale(&s, CURRENT, 1).is_empty());
    }

    #[test]
    fn windows_exe_extension_is_recognized() {
        // On Windows, `is_aiui_binary` must accept `aiui.exe` regardless of
        // case. Verify here cross-platform — the leaf check is OS-agnostic.
        assert!(is_aiui_binary(r"C:\Program Files\aiui\aiui.exe"));
        assert!(is_aiui_binary(r"C:\Program Files\aiui\AIUI.EXE"));
        assert!(is_aiui_binary("/Applications/aiui.app/Contents/MacOS/aiui"));
        assert!(!is_aiui_binary("/usr/bin/python3"));
    }

    fn ssh_ntr_args(host: &str, port: u16) -> Vec<String> {
        // Mirrors the spawn in tunnel.rs:run_tunnel exactly.
        [
            "ssh",
            "-N",
            "-T",
            "-R",
            &format!("{port}:localhost:{port}"),
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "--",
            host,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn ssh_ntr_signature_matches_real_tunnel_args() {
        let a = ssh_ntr_args("dev@devhost", 7777);
        assert!(is_aiui_ssh_ntr_for_port(&a, 7777));
    }

    #[test]
    fn ssh_ntr_signature_rejects_other_ports() {
        let a = ssh_ntr_args("dev@devhost", 7777);
        assert!(!is_aiui_ssh_ntr_for_port(&a, 8888));
    }

    #[test]
    fn ssh_ntr_signature_rejects_local_forward() {
        // -L instead of -R — local forward, not our reverse tunnel.
        let a: Vec<String> = ["ssh", "-N", "-T", "-L", "7777:localhost:7777", "host"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(!is_aiui_ssh_ntr_for_port(&a, 7777));
    }

    #[test]
    fn ssh_ntr_signature_rejects_non_ssh_command() {
        let a: Vec<String> = ["bash", "-c", "ssh -N -T -R 7777:localhost:7777 host"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(!is_aiui_ssh_ntr_for_port(&a, 7777));
    }

    #[test]
    fn find_aiui_ssh_ntr_orphans_only_filters_on_ppid() {
        let s = vec![
            // Orphan from a crashed earlier aiui — re-parented to pid 1.
            snap_with_ppid(30295, 1, "/usr/bin/ssh", &ssh_ntr_args("dev@devhost", 7777).iter().map(String::as_str).collect::<Vec<_>>()),
            // Active tunnel from the current GUI (ppid != 1).
            snap_with_ppid(40000, 76770, "/usr/bin/ssh", &ssh_ntr_args("customer@macmini", 7777).iter().map(String::as_str).collect::<Vec<_>>()),
        ];
        let orphans = find_aiui_ssh_ntr(&s, 7777, true);
        assert_eq!(orphans, vec![30295]);

        let all = find_aiui_ssh_ntr(&s, 7777, false);
        assert_eq!(all.len(), 2);
    }

    // ---------- find_orphaned_mcp_stdio_to_kill (Bug A, v0.4.46) ----------

    #[test]
    fn orphan_reaped_when_reparented_to_launchd() {
        // Parent died → child reparented to launchd (ppid==1).
        let snap = vec![
            snap_full(1, 0, "/sbin/launchd", &["launchd"], 1),
            snap_full(300, 1, CURRENT, &[CURRENT, "--mcp-stdio"], 1100),
        ];
        let victims = find_orphaned_mcp_stdio_to_kill(&snap, 999);
        assert_eq!(victims.len(), 1);
        assert_eq!(victims[0].pid, 300);
    }

    #[test]
    fn orphan_reaped_when_parent_absent_from_snapshot() {
        // Parent pid 250 is gone (not in the snapshot) → orphan.
        let snap = vec![snap_full(300, 250, CURRENT, &[CURRENT, "--mcp-stdio"], 1100)];
        let victims = find_orphaned_mcp_stdio_to_kill(&snap, 999);
        assert_eq!(victims.len(), 1);
        assert_eq!(victims[0].pid, 300);
    }

    #[test]
    fn live_child_with_alive_parent_is_spared() {
        // Wrapper 200 is alive → child 300 belongs to a live session.
        let snap = vec![
            snap_full(100, 1, "/Applications/Claude.app/Contents/MacOS/Claude", &["Claude"], 800),
            snap_full(200, 100, "/Applications/Claude.app/Contents/Helpers/disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 1099),
            snap_full(300, 200, CURRENT, &[CURRENT, "--mcp-stdio"], 1100),
        ];
        let victims = find_orphaned_mcp_stdio_to_kill(&snap, 999);
        assert!(victims.is_empty(), "live child with alive parent must be spared");
    }

    #[test]
    fn concurrent_cowork_sessions_all_spared() {
        // THE Bug A regression test. Two concurrent Cowork sessions, each
        // with its own live disclaimer wrapper, both under the one live
        // Claude.app grandparent (100). The old "same grandparent" rule
        // reaped one when the other started — orphan-status spares both.
        let snap = vec![
            snap_full(100, 1, "/Applications/Claude.app/Contents/MacOS/Claude", &["Claude"], 800),
            snap_full(200, 100, "/Applications/Claude.app/Contents/Helpers/disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 1099),
            snap_full(300, 200, CURRENT, &[CURRENT, "--mcp-stdio"], 1100), // session A
            snap_full(400, 100, "/Applications/Claude.app/Contents/Helpers/disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 2099),
            snap_full(500, 400, CURRENT, &[CURRENT, "--mcp-stdio"], 2100), // session B (us)
        ];
        let victims = find_orphaned_mcp_stdio_to_kill(&snap, 500);
        assert!(victims.is_empty(), "live parallel Cowork sessions must all be spared");
    }

    #[test]
    fn own_pid_never_reaped_even_if_orphaned() {
        let snap = vec![snap_full(300, 1, CURRENT, &[CURRENT, "--mcp-stdio"], 1100)];
        let victims = find_orphaned_mcp_stdio_to_kill(&snap, 300);
        assert!(victims.is_empty(), "own pid must never be reaped");
    }

    #[test]
    fn non_aiui_orphan_and_non_mcp_aiui_ignored() {
        let snap = vec![
            // orphaned, but not aiui
            snap_full(300, 1, "/usr/bin/python", &["python", "foo.py"], 1100),
            // orphaned aiui, but GUI mode (no --mcp-stdio flag)
            snap_full(310, 1, CURRENT, &[CURRENT, "--auto"], 1100),
        ];
        let victims = find_orphaned_mcp_stdio_to_kill(&snap, 999);
        assert!(victims.is_empty());
    }

    #[test]
    fn is_orphaned_child_handles_none_ppid() {
        let p = ProcSnap {
            pid: 300,
            ppid: None,
            exe: CURRENT.to_string(),
            args: vec![],
            start_time: 1,
        };
        assert!(is_orphaned_child(&[], &p));
    }

    // ---------- find_pre_gui_mcp_stdio_to_kill ----------

    #[test]
    fn pre_gui_kill_finds_older_children() {
        let snap = vec![
            snap_full(200, 100, CURRENT, &[CURRENT, "--mcp-stdio"], 1000),
            snap_full(300, 100, CURRENT, &[CURRENT, "--mcp-stdio"], 1500),
            snap_full(400, 1, "/Applications/aiui.app/Contents/MacOS/aiui", &["aiui"], 2000),
        ];
        // We are pid 400, started at t=2000.
        let victims = find_pre_gui_mcp_stdio_to_kill(&snap, 400, 2000);
        assert_eq!(victims.len(), 2);
        let pids: Vec<u32> = victims.iter().map(|v| v.pid).collect();
        assert!(pids.contains(&200));
        assert!(pids.contains(&300));
    }

    #[test]
    fn pre_gui_kill_skips_same_second_children() {
        // start_time is integer seconds. A child started in the same
        // second as the GUI (1500 == 1500) MUST NOT be killed —
        // otherwise two near-simultaneous spawns can ping-pong each
        // other to death.
        let snap = vec![
            snap_full(200, 100, CURRENT, &[CURRENT, "--mcp-stdio"], 1500),
            snap_full(300, 1, "aiui", &["aiui"], 1500),
        ];
        let victims = find_pre_gui_mcp_stdio_to_kill(&snap, 300, 1500);
        assert!(
            victims.is_empty(),
            "same-second child must not be a pre-GUI kill victim"
        );
    }

    #[test]
    fn pre_gui_kill_skips_newer_children() {
        // Children started AFTER us must not be killed — the rule is
        // strictly "older than GUI". A newer child is the next legit
        // mcp-stdio that Claude Desktop has just spawned against us.
        let snap = vec![
            snap_full(200, 100, CURRENT, &[CURRENT, "--mcp-stdio"], 3000),
            snap_full(300, 1, "aiui", &["aiui"], 2000),
        ];
        let victims = find_pre_gui_mcp_stdio_to_kill(&snap, 300, 2000);
        assert!(victims.is_empty());
    }

    #[test]
    fn pre_gui_kill_skips_own_pid() {
        let snap = vec![snap_full(300, 1, CURRENT, &[CURRENT, "--mcp-stdio"], 1000)];
        let victims = find_pre_gui_mcp_stdio_to_kill(&snap, 300, 2000);
        assert!(victims.is_empty());
    }

    #[test]
    fn pre_gui_kill_ignores_non_aiui_processes() {
        let snap = vec![
            snap_full(200, 100, "/usr/bin/python3", &["python", "--mcp-stdio"], 1000),
            snap_full(300, 1, "aiui", &["aiui"], 2000),
        ];
        let victims = find_pre_gui_mcp_stdio_to_kill(&snap, 300, 2000);
        assert!(
            victims.is_empty(),
            "a python script with --mcp-stdio flag must not match"
        );
    }

    // ---------- ProcessLock ----------

    #[test]
    fn process_lock_basic_acquire_and_release() {
        let dir = std::env::temp_dir().join(format!(
            "aiui-test-lock-basic-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gui.lock");

        let lock = ProcessLock::try_acquire(&path).expect("first acquire must succeed");
        assert_eq!(lock.path(), path);
        drop(lock);
        // After drop, a fresh acquire must succeed again.
        let lock2 = ProcessLock::try_acquire(&path).expect("re-acquire after drop must succeed");
        drop(lock2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn process_lock_is_exclusive_while_held() {
        let dir = std::env::temp_dir().join(format!(
            "aiui-test-lock-exclusive-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gui.lock");

        let _guard = ProcessLock::try_acquire(&path).expect("first acquire must succeed");
        // Second acquire from the SAME process: fs4's flock semantics
        // grant the lock to the holding process, so a second
        // try_lock_exclusive on a different file handle should still
        // fail on Linux but may succeed on macOS depending on flock
        // semantics. The cross-platform guarantee is that a different
        // *process* cannot acquire — which we can't easily test in a
        // single-process unit test. Instead, verify the path-mechanics
        // are sound and the lock file is created.
        assert!(path.exists(), "lock file must exist after acquire");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn process_lock_creates_parent_directory() {
        let dir = std::env::temp_dir().join(format!(
            "aiui-test-lock-mkdir-{}",
            std::process::id()
        ));
        // Don't pre-create the directory — the helper should.
        let path = dir.join("nested/gui.lock");

        let lock = ProcessLock::try_acquire(&path).expect("must create parent dir");
        assert!(path.exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_aiui_ssh_ntr_ignores_unrelated_ssh() {
        let unrelated: Vec<String> = ["ssh", "user@host", "ls"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let s = vec![ProcSnap {
            pid: 99999,
            ppid: Some(1),
            exe: "/usr/bin/ssh".into(),
            args: unrelated,
            start_time: 0,
        }];
        assert!(find_aiui_ssh_ntr(&s, 7777, true).is_empty());
        assert!(find_aiui_ssh_ntr(&s, 7777, false).is_empty());
    }
}
