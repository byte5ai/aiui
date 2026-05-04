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

/// Lightweight snapshot of one process — what we need for both filters.
#[derive(Debug, Clone)]
struct ProcSnap {
    pid: u32,
    exe: String,
    args: Vec<String>,
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
                exe,
                args,
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
            exe: exe.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
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
}
