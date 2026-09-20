//! Cross-platform helpers for spawning child processes from the GUI.
//!
//! Why this exists: aiui ships as a `windows_subsystem = "windows"` GUI
//! binary on Windows, which means it has no console attached. When such a
//! binary calls `std::process::Command::new(...)` for any non-GUI helper
//! (`tasklist`, `ssh`, `scp`, `cmd /C start …`), Windows defaults to
//! allocating a fresh console window for the child. The console flashes on
//! screen for a fraction of a second and disappears when the child exits.
//!
//! For periodically-polled probes like `is_claude_desktop_running` (driven
//! by the settings window's status refresh), the user sees a constant strobe
//! of black command-prompt rectangles. First reported by an external Windows
//! tester on 2026-05-07.
//!
//! The fix is to set the `CREATE_NO_WINDOW` (0x08000000) creation flag on
//! every child we don't actually want a console for. The flag only
//! suppresses the *console-window allocation*; pipes set up via
//! `Stdio::piped()` / `.output()` still capture the child's stdout and
//! stderr exactly as they would without the flag. So call sites that
//! currently parse a child's output (e.g. `tasklist`, `ssh`, `scp`,
//! `taskkill`) can route through `no_window(...)` without losing any
//! information they were relying on — they only lose the visible window.
//!
//! On Unix the helpers are no-ops (the flag has no analogue and the GUI
//! process model is different — no spurious terminals appear).

#![allow(dead_code)]

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Apply `CREATE_NO_WINDOW` to a `std::process::Command` on Windows.
/// No-op on other platforms. Returns the same `&mut Command` so it
/// chains naturally inside builder expressions.
pub fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Same as `no_window`, but for `tokio::process::Command`. Tokio's
/// `creation_flags` is an inherent method on Windows targets and absent
/// on Unix, so this wrapper hides the cfg.
pub fn no_window_tokio(
    cmd: &mut tokio::process::Command,
) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Detach a child from the caller's standard streams, then spawn it.
///
/// Mandatory for the auto-resurrect spawn (`lifetime::spawn_gui_detached`):
/// the caller may be `aiui --mcp-stdio`, whose stdin and stdout *are* the
/// host's JSON-RPC pipes. `std::process::Command` defaults every stream to
/// `Stdio::inherit()`, so a plain `.spawn()` hands those pipe handles to the
/// GUI — which then writes its `tauri_plugin_log` output straight into the
/// host's framing stream (#181) and keeps the write end open for its whole
/// life, so the host never sees EOF when the mcp-stdio child exits.
///
/// All three streams are nulled, not just stdout: an inherited stdin keeps
/// the read end of the host's request pipe alive in the GUI, and any future
/// reader there would steal JSON-RPC frames.
///
/// Note this is *not* [`no_window`]. `CREATE_NO_WINDOW` suppresses
/// console-window allocation and has no effect whatsoever on handle
/// inheritance; it would fix nothing here. The two console flags are also
/// mutually exclusive, and aiui is `windows_subsystem = "windows"` anyway,
/// so no console is allocated in the first place.
pub fn spawn_detached(
    cmd: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    /// #181 regression guard for the MCP-stream pollution: a stdout the
    /// caller already configured must not survive into the spawned child.
    /// Modelled with a temp file standing in for the host's JSON-RPC pipe.
    ///
    /// Unix-gated so it runs on the macOS CI leg — the Windows test binary
    /// is compiled but not executed (#141), and the crate has no
    /// dev-dependencies, so this stays std-only.
    #[cfg(unix)]
    #[test]
    fn spawn_detached_overrides_a_stdout_the_caller_configured() {
        use std::io::Read;
        use std::process::{Command, Stdio};

        let path = std::env::temp_dir().join(format!(
            "aiui-spawn-detached-{}.out",
            std::process::id()
        ));
        let file = std::fs::File::create(&path).expect("create temp stdout");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo SENTINEL").stdout(Stdio::from(file));

        let status = super::spawn_detached(&mut cmd)
            .expect("spawn")
            .wait()
            .expect("wait");
        assert!(status.success(), "child should have run");

        let mut out = String::new();
        std::fs::File::open(&path)
            .expect("reopen temp stdout")
            .read_to_string(&mut out)
            .expect("read temp stdout");
        let _ = std::fs::remove_file(&path);

        assert!(
            out.is_empty(),
            "the caller's stdout must be replaced, not inherited; got {out:?}"
        );
    }
}
