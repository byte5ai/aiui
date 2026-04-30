//! Kill stale `aiui --mcp-stdio` children left over from older app versions.
//!
//! Context: Claude Desktop spawns `aiui --mcp-stdio` once and keeps it alive
//! for the whole Claude Desktop session. If the user drags a new `aiui.app`
//! over the old one, those already-spawned children keep running — with the
//! *old* binary. Their lifetime-socket logic may be pre-auto-resurrect (≤
//! v0.2.5) or otherwise incompatible, so the user ends up with a stale MCP
//! server that refuses to reconnect to the new GUI.
//!
//! Two complementary mechanisms exist:
//!
//!  1. **GUI-side sweep** (`kill_stale_mcp_stdio_children`): on every GUI
//!     startup we scan for `aiui --mcp-stdio` processes whose executable
//!     path differs from ours and SIGTERM them. This only catches the case
//!     where the *path* changed — useless when the user dragged a new
//!     `aiui.app` over the old one in place.
//!
//!  2. **Subprocess-side self-check** (`disk_version_if_stale`): every
//!     `--mcp-stdio` invocation reads `CFBundleShortVersionString` from
//!     the on-disk `Info.plist` two directories up from `argv[0]` and
//!     compares it with `CARGO_PKG_VERSION` baked in at compile time. If
//!     they disagree, the in-memory binary is stale — the bundle on disk
//!     was replaced after this process loaded — and we exit so Claude
//!     Desktop respawns us against the fresh binary.
//!
//! Together those two layers catch the in-place-replace scenario that
//! produced the 2026-04-30 form-tool-call crash: a Claude-Desktop-spawned
//! mcp-stdio child kept the previous version in memory across an
//! updater-driven replacement of `/Applications/aiui.app`.
//!
//! macOS-specific: we use `ps -axo pid=,command=` to enumerate processes
//! (there is no /proc on macOS). The executable path is the first whitespace-
//! delimited token of `command`.
//!
//! Safety: we never kill our own pid. If the current binary path can't be
//! determined, we skip the sweep entirely.
//!
//! Idempotent: called once per GUI startup. Running it on a clean system is
//! a no-op.

use crate::logging::trace;
use std::path::PathBuf;
use std::process::Command;

/// A stale `aiui --mcp-stdio` process that should be terminated.
#[derive(Debug, PartialEq, Eq)]
struct StaleChild {
    pid: u32,
    exe: String,
}

/// Parse `ps -axo pid=,command=` output and return the stale-child candidates
/// — processes running `aiui --mcp-stdio` under any executable path other
/// than `current_exe_path`, excluding `own_pid`.
fn find_stale(ps_stdout: &str, current_exe_path: &str, own_pid: u32) -> Vec<StaleChild> {
    let mut out = Vec::new();
    for line in ps_stdout.lines() {
        let line = line.trim_start();
        let Some((pid_str, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let rest = rest.trim_start();
        if !rest.contains("--mcp-stdio") {
            continue;
        }
        let exe = rest.split_whitespace().next().unwrap_or("");
        // Narrow to our binary family — any path ending in /aiui that's
        // running with --mcp-stdio. This skips random unrelated processes
        // that happen to mention "--mcp-stdio" in their argv.
        if !exe.ends_with("/aiui") && exe != "aiui" {
            continue;
        }
        if exe == current_exe_path {
            continue;
        }
        out.push(StaleChild {
            pid,
            exe: exe.to_string(),
        });
    }
    out
}

/// Scan for stale `aiui --mcp-stdio` processes and SIGTERM the ones whose
/// executable path differs from `current_exe_path`. Returns the number of
/// processes killed.
pub fn kill_stale_mcp_stdio_children(current_exe_path: &str) -> usize {
    let own_pid = std::process::id();

    let out = match Command::new("ps").args(["-axo", "pid=,command="]).output() {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            trace(&format!(
                "housekeeping: ps exited {:?} ({} bytes stderr)",
                o.status.code(),
                o.stderr.len()
            ));
            return 0;
        }
        Err(e) => {
            trace(&format!("housekeeping: ps failed to start: {e}"));
            return 0;
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stale = find_stale(&stdout, current_exe_path, own_pid);

    for child in &stale {
        trace(&format!(
            "housekeeping: killing stale mcp-stdio child pid={} exe={}",
            child.pid, child.exe
        ));
        // SIGTERM (15). Claude Desktop notices the broken pipe and respawns
        // with the current config, which now points at our binary.
        let _ = Command::new("kill").arg(child.pid.to_string()).output();
    }

    if !stale.is_empty() {
        trace(&format!(
            "housekeeping: terminated {} stale mcp-stdio child(ren)",
            stale.len()
        ));
    }
    stale.len()
}

/// Pure decision: given our compile-time version string and the version
/// string read from the on-disk `Info.plist`, return `true` when this
/// in-memory binary is stale (i.e. should exit so it can be respawned).
///
/// Empty / whitespace `disk` is treated as "unknown" → not stale: better
/// to keep running than abort a working subprocess on a transient
/// `plutil` glitch.
pub(crate) fn is_disk_version_stale(own: &str, disk: &str) -> bool {
    let disk = disk.trim();
    !disk.is_empty() && disk != own
}

/// True iff the bundle on disk (one bundle level up from `argv[0]`)
/// reports a `CFBundleShortVersionString` that differs from our own
/// compile-time `CARGO_PKG_VERSION`. Returns the on-disk version when
/// stale so the caller can log it; `None` when fresh, when running
/// outside an `.app` bundle (dev build, `cargo run`), or when the
/// lookup itself fails (no `Info.plist`, `plutil` missing, …).
///
/// Self-detection at the subprocess side is what closes the gap that
/// the path-based GUI sweep can't see: an in-place `.app` replacement
/// leaves the running child with stale code at the unchanged path.
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

/// Find every `aiui --mcp-stdio` process regardless of executable path,
/// excluding `own_pid`. Used for the uninstall flow where we want to take
/// down ALL children — keeping any of them alive would re-launch the GUI
/// via `mcp_attach`'s auto-resurrect path the moment we exit, defeating
/// the whole point of "uninstall + drag .app to trash".
fn find_all_children(ps_stdout: &str, own_pid: u32) -> Vec<StaleChild> {
    let mut out = Vec::new();
    for line in ps_stdout.lines() {
        let line = line.trim_start();
        let Some((pid_str, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let rest = rest.trim_start();
        if !rest.contains("--mcp-stdio") {
            continue;
        }
        let exe = rest.split_whitespace().next().unwrap_or("");
        if !exe.ends_with("/aiui") && exe != "aiui" {
            continue;
        }
        out.push(StaleChild {
            pid,
            exe: exe.to_string(),
        });
    }
    out
}

/// Sibling of `kill_stale_mcp_stdio_children` that doesn't filter by
/// executable path — every running `aiui --mcp-stdio` (other than our
/// own pid) gets SIGTERM'd. Bound to the uninstall flow (#72): without
/// this, the auto-resurrect loop in `mcp_attach` would relaunch the GUI
/// the moment we call `app.exit(0)`.
pub fn kill_all_mcp_stdio_children() -> usize {
    let own_pid = std::process::id();

    let out = match Command::new("ps").args(["-axo", "pid=,command="]).output() {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            trace(&format!(
                "housekeeping: ps exited {:?} ({} bytes stderr)",
                o.status.code(),
                o.stderr.len()
            ));
            return 0;
        }
        Err(e) => {
            trace(&format!("housekeeping: ps failed to start: {e}"));
            return 0;
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let children = find_all_children(&stdout, own_pid);

    for child in &children {
        trace(&format!(
            "housekeeping: killing mcp-stdio child pid={} exe={} (uninstall sweep)",
            child.pid, child.exe
        ));
        let _ = Command::new("kill").arg(child.pid.to_string()).output();
    }

    if !children.is_empty() {
        trace(&format!(
            "housekeeping: terminated {} mcp-stdio child(ren) for uninstall",
            children.len()
        ));
    }
    children.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: &str = "/Applications/aiui.app/Contents/MacOS/aiui";

    #[test]
    fn skips_empty_and_garbage_lines() {
        assert!(find_stale("", CURRENT, 1).is_empty());
        assert!(find_stale("  \n\t\n", CURRENT, 1).is_empty());
        assert!(find_stale("not a pid line", CURRENT, 1).is_empty());
    }

    #[test]
    fn skips_unrelated_processes() {
        let ps = "\
          12345 /usr/bin/python3 some_script.py --mcp-stdio\n\
          23456 /opt/homebrew/bin/uv tool uvx aiui-mcp\n\
          34567 /bin/zsh -c echo hello\n\
        ";
        assert!(find_stale(ps, CURRENT, 1).is_empty());
    }

    #[test]
    fn skips_current_binary() {
        let ps = format!("99999 {CURRENT} --mcp-stdio\n");
        assert!(find_stale(&ps, CURRENT, 1).is_empty());
    }

    #[test]
    fn skips_own_pid_even_if_path_differs() {
        let ps = "12345 /old/path/aiui --mcp-stdio\n";
        assert!(find_stale(ps, CURRENT, 12345).is_empty());
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
        // If `plutil` returns nothing — bundle missing, dev build,
        // permissions issue — we'd rather keep running than abort.
        // The GUI-side sweep is the safety net for that path.
        assert!(!is_disk_version_stale("0.4.26", ""));
        assert!(!is_disk_version_stale("0.4.26", "   "));
        assert!(!is_disk_version_stale("0.4.26", "\n\n"));
    }

    #[test]
    fn finds_stale_child_with_different_path() {
        let ps = "\
          12345 /old/path/aiui --mcp-stdio\n\
          23456 /Applications/aiui.app/Contents/MacOS/aiui --mcp-stdio\n\
        ";
        let stale = find_stale(ps, CURRENT, 1);
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
        let ps = "\
          100 /a/aiui --mcp-stdio\n\
          200 /b/aiui --mcp-stdio --extra\n\
          300 /Applications/aiui.app/Contents/MacOS/aiui --mcp-stdio\n\
        ";
        let stale = find_stale(ps, CURRENT, 1);
        assert_eq!(stale.len(), 2);
        assert_eq!(stale[0].pid, 100);
        assert_eq!(stale[1].pid, 200);
    }

    #[test]
    fn ignores_aiui_gui_processes_without_mcp_stdio_flag() {
        // The GUI process itself runs the same binary but without
        // `--mcp-stdio`. Must not be killed.
        let ps = format!("42 {CURRENT}\n43 /old/path/aiui\n");
        assert!(find_stale(&ps, CURRENT, 1).is_empty());
    }

    #[test]
    fn tolerates_leading_whitespace_from_ps() {
        // ps pads pid column with leading spaces.
        let ps = "    12345 /old/path/aiui --mcp-stdio\n";
        let stale = find_stale(ps, CURRENT, 1);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].pid, 12345);
    }
}
