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
use sysinfo::{ProcessRefreshKind, RefreshKind, Signal, System};

#[cfg(target_os = "macos")]
use std::path::PathBuf;
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

/// Resolve the grandparent (= parent's parent) PID of `pid` in the
/// snapshot. Returns `None` if the chain breaks at any step — caller
/// treats that as "unknown lineage, do not act".
fn grandparent_pid(snap: &[ProcSnap], pid: u32) -> Option<u32> {
    let me = snap.iter().find(|p| p.pid == pid)?;
    let ppid = me.ppid?;
    let parent = snap.iter().find(|p| p.pid == ppid)?;
    parent.ppid
}

/// Filter: other `aiui --mcp-stdio` children spawned by the same MCP
/// client (= same grandparent in the process tree, typically the Claude
/// Desktop or Claude Code App process) and started *before* us. Pure
/// function over a snapshot — the `(own_pid, own_grandparent, own_start)`
/// triple comes from the caller so tests don't need to spoof
/// `std::process::id()`.
///
/// "Same grandparent" was picked over "same parent" because every
/// mcp-stdio on macOS sits one level below a Claude.app helper wrapper
/// (`disclaimer`); each spawn gets its own wrapper, so PPIDs differ,
/// but the wrappers themselves are all children of the same Claude.app
/// process.
///
/// "Started before us" enforces a strict newer-kills-older rule. Two
/// mcp-stdios spawned at the same instant from the same parent would
/// otherwise see each other as "older" and tear each other down.
/// v0.4.42.
fn find_sibling_mcp_stdio_to_kill(
    snap: &[ProcSnap],
    own_pid: u32,
    own_grandparent: Option<u32>,
    own_start_time: u64,
) -> Vec<StaleChild> {
    let Some(own_gp) = own_grandparent else {
        return Vec::new();
    };
    snap.iter()
        .filter(|p| p.pid != own_pid)
        .filter(|p| has_mcp_stdio_flag(&p.args))
        .filter(|p| is_aiui_binary(&p.exe))
        .filter(|p| p.start_time < own_start_time)
        .filter(|p| grandparent_pid(snap, p.pid) == Some(own_gp))
        .map(|p| StaleChild {
            pid: p.pid,
            exe: p.exe.clone(),
        })
        .collect()
}

/// Kill any older `aiui --mcp-stdio` children spawned by the *same* MCP
/// client as us. Called once at mcp-stdio startup, after the
/// `disk_version_if_stale` self-check. Returns the number of siblings
/// terminated.
///
/// Why this exists: Claude Desktop has been observed (2026-05-16) to
/// occasionally spawn a fresh `aiui --mcp-stdio` child while leaving
/// the previous one running. Both children attach to the lifetime
/// socket, both register their prompt list, and Claude Desktop's
/// internal slash-command routing then can't decide which child owns
/// `prompts/get` — the slash-command fails with "kein erkannter
/// Befehl" even though the prompt list itself was loaded correctly.
/// The aiui-side fix is the strict newer-kills-older policy here: the
/// fresh child wins, the stale one gets SIGTERM, Claude Desktop is
/// left with exactly one mcp-stdio per app instance.
///
/// We also terminate the immediate parent (the
/// `Claude.app/Contents/Helpers/disclaimer` wrapper on macOS) of the
/// older sibling so it doesn't immediately respawn a replacement and
/// trigger the race again. The grandparent — the Claude.app process
/// itself — is never touched.
///
/// Safety: if our own lineage can't be fully resolved (e.g. we're
/// running standalone from a terminal, no MCP-client grandparent), the
/// function is a no-op. We never broadly sweep all aiui-mcp-stdio
/// children — `kill_all_mcp_stdio_children` is the uninstall-only path
/// for that.
pub fn kill_sibling_mcp_stdio_with_same_grandparent() -> usize {
    let own_pid = std::process::id();
    let snap = snapshot_processes();
    let own_grandparent = grandparent_pid(&snap, own_pid);
    let own_start_time = snap
        .iter()
        .find(|p| p.pid == own_pid)
        .map(|p| p.start_time)
        .unwrap_or(0);

    let siblings = find_sibling_mcp_stdio_to_kill(
        &snap,
        own_pid,
        own_grandparent,
        own_start_time,
    );

    if siblings.is_empty() {
        return 0;
    }

    let gp_str = own_grandparent
        .map(|gp| gp.to_string())
        .unwrap_or_else(|| "<unknown>".into());

    for sibling in &siblings {
        // Find the sibling's immediate parent (typically the disclaimer
        // wrapper on macOS) so we can SIGTERM it as well — without
        // that, Claude Desktop's spawn-supervisor would re-run a fresh
        // child against the just-killed pid and we'd be back to two.
        let sibling_ppid = snap
            .iter()
            .find(|p| p.pid == sibling.pid)
            .and_then(|p| p.ppid);

        trace(&format!(
            "housekeeping: killing older sibling mcp-stdio pid={} exe={} \
             (same grandparent={} as own pid={})",
            sibling.pid, sibling.exe, gp_str, own_pid
        ));
        terminate_pid(sibling.pid);

        // Belt-and-braces guard: never SIGTERM the grandparent itself —
        // that would be the Claude Desktop / Claude Code app and would
        // take our own current MCP session down with it.
        if let Some(wrapper_pid) = sibling_ppid {
            if Some(wrapper_pid) != own_grandparent && wrapper_pid != own_pid {
                trace(&format!(
                    "housekeeping: also terminating sibling wrapper pid={wrapper_pid}"
                ));
                terminate_pid(wrapper_pid);
            }
        }
    }
    let n = siblings.len();
    trace(&format!(
        "housekeeping: terminated {n} older sibling mcp-stdio child(ren) on startup \
         (grandparent={gp_str})"
    ));
    n
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

    // ---------- find_sibling_mcp_stdio_to_kill ----------

    /// Build the canonical Claude-Desktop process tree:
    ///   100 (Claude.app)
    ///   ├─ 200 (disclaimer wrapper)
    ///   │    └─ 300 (aiui --mcp-stdio, older, start=1100)
    ///   └─ 400 (disclaimer wrapper)
    ///        └─ 500 (aiui --mcp-stdio, newer, start=2100)
    fn canonical_claude_desktop_tree() -> Vec<ProcSnap> {
        vec![
            snap_full(100, 1, "/Applications/Claude.app/Contents/MacOS/Claude", &["Claude"], 800),
            snap_full(200, 100, "/Applications/Claude.app/Contents/Helpers/disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 1099),
            snap_full(300, 200, CURRENT, &[CURRENT, "--mcp-stdio"], 1100),
            snap_full(400, 100, "/Applications/Claude.app/Contents/Helpers/disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 2099),
            snap_full(500, 400, CURRENT, &[CURRENT, "--mcp-stdio"], 2100),
        ]
    }

    #[test]
    fn grandparent_resolves_through_disclaimer_wrapper() {
        let snap = canonical_claude_desktop_tree();
        assert_eq!(grandparent_pid(&snap, 500), Some(100));
        assert_eq!(grandparent_pid(&snap, 300), Some(100));
    }

    #[test]
    fn grandparent_is_none_when_chain_breaks() {
        // Orphan with ppid=1 but no entry for pid=1 in the snapshot.
        let snap = vec![snap_full(500, 1, CURRENT, &[CURRENT, "--mcp-stdio"], 2100)];
        assert_eq!(grandparent_pid(&snap, 500), None);
    }

    #[test]
    fn sibling_kill_finds_older_same_grandparent() {
        let snap = canonical_claude_desktop_tree();
        // We are pid 500 (the newer mcp-stdio), grandparent 100, start 2100.
        let victims = find_sibling_mcp_stdio_to_kill(&snap, 500, Some(100), 2100);
        assert_eq!(victims.len(), 1);
        assert_eq!(victims[0].pid, 300);
    }

    #[test]
    fn sibling_kill_skips_newer_siblings() {
        let snap = canonical_claude_desktop_tree();
        // We are pid 300 (the older mcp-stdio). The newer one (500) must
        // NOT show up — otherwise two simultaneously-started duplicates
        // would tear each other down.
        let victims = find_sibling_mcp_stdio_to_kill(&snap, 300, Some(100), 1100);
        assert!(victims.is_empty(), "older must not target newer siblings");
    }

    #[test]
    fn sibling_kill_ignores_other_mcp_client() {
        // Two completely different MCP clients on the same Mac:
        //   100 (Claude Desktop) → 200 (disclaimer) → 300 (aiui --mcp-stdio, older)
        //   150 (Claude Code CLI) → 250 (bash)       → 350 (aiui --mcp-stdio, newer — us)
        let snap = vec![
            snap_full(100, 1, "/Applications/Claude.app/Contents/MacOS/Claude", &["Claude"], 800),
            snap_full(200, 100, "/Applications/Claude.app/Contents/Helpers/disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 1099),
            snap_full(300, 200, CURRENT, &[CURRENT, "--mcp-stdio"], 1100),
            snap_full(150, 1, "/opt/homebrew/bin/claude", &["claude"], 1500),
            snap_full(250, 150, "/bin/bash", &["bash", "-c", "..."], 1599),
            snap_full(350, 250, CURRENT, &[CURRENT, "--mcp-stdio"], 1600),
        ];
        // We are pid 350; grandparent is 150. Sibling 300's grandparent is
        // 100 — different MCP client, must not be killed.
        let victims = find_sibling_mcp_stdio_to_kill(&snap, 350, Some(150), 1600);
        assert!(
            victims.is_empty(),
            "must not kill mcp-stdio of a different MCP client (different grandparent)"
        );
    }

    #[test]
    fn sibling_kill_noop_when_own_grandparent_unknown() {
        let snap = canonical_claude_desktop_tree();
        // Caller couldn't resolve our grandparent — must be a no-op
        // regardless of what siblings exist, so we don't broadly sweep.
        let victims = find_sibling_mcp_stdio_to_kill(&snap, 500, None, 2100);
        assert!(victims.is_empty());
    }

    #[test]
    fn sibling_kill_skips_own_pid_even_if_older() {
        let snap = canonical_claude_desktop_tree();
        // Caller passes our own pid as `own_pid` — we must never
        // include ourselves even if start_time math says we're older
        // than some hypothetical caller frame.
        let victims = find_sibling_mcp_stdio_to_kill(&snap, 500, Some(100), 9999);
        assert_eq!(victims.iter().filter(|v| v.pid == 500).count(), 0);
    }

    #[test]
    fn sibling_kill_skips_unrelated_aiui_mcp_in_tree() {
        // Two old aiui-mcp-stdio children, but only one is in our
        // grandparent lineage. The other should not be touched.
        let snap = vec![
            snap_full(100, 1, "Claude", &["Claude"], 800),
            snap_full(200, 100, "disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 1099),
            snap_full(300, 200, CURRENT, &[CURRENT, "--mcp-stdio"], 1100),
            // unrelated tree:
            snap_full(700, 1, "OtherApp", &["OtherApp"], 800),
            snap_full(750, 700, "wrapper", &["wrapper", CURRENT, "--mcp-stdio"], 1099),
            snap_full(770, 750, CURRENT, &[CURRENT, "--mcp-stdio"], 1100),
            // us:
            snap_full(400, 100, "disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 2099),
            snap_full(500, 400, CURRENT, &[CURRENT, "--mcp-stdio"], 2100),
        ];
        let victims = find_sibling_mcp_stdio_to_kill(&snap, 500, Some(100), 2100);
        assert_eq!(victims.len(), 1);
        assert_eq!(victims[0].pid, 300);
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
