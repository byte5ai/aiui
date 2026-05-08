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
