//! Kill stale `aiui --mcp-stdio` children left over from older app versions.
//!
//! Context: Claude Desktop spawns `aiui --mcp-stdio` once and keeps it alive
//! for the whole Claude Desktop session. If the user drags a new `aiui.app`
//! over the old one, those already-spawned children keep running — with the
//! *old* binary. Their lifetime-socket logic may be pre-auto-resurrect (≤
//! v0.2.5) or otherwise incompatible, so the user ends up with a stale MCP
//! server that refuses to reconnect to the new GUI.
//!
//! On every GUI startup we therefore scan for `aiui --mcp-stdio` processes
//! whose executable path differs from ours and SIGTERM them. Claude Desktop
//! will respawn them against the freshly patched config — pointing at the
//! new binary.
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
