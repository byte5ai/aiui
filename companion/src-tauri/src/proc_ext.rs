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

/// Wall-clock bound for a short setup-side ssh/scp step (mkdir, token copy,
/// reachability probe, cleanup). Review B1-08.
pub const SSH_STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Wall-clock bound for a setup-side ssh step that runs a script on the
/// remote (config patch, `uvx aiui-mcp` probe through a login shell).
pub const SSH_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// `Command::output()` with a wall-clock bound, optionally feeding `stdin`
/// (review B1-08).
///
/// Every setup-side `ssh`/`scp` call was a bare `output()` inside an async
/// Tauri command. `ConnectTimeout` bounds only the TCP connect — a host whose
/// NFS home is down accepts the connection and then wedges in `chdir($HOME)`,
/// so Uninstall never finished and Settings stayed `busy` with every button
/// disabled. On expiry the child is killed and the error kind is `TimedOut`.
///
/// stdout and stderr are drained on their own threads while waiting, so a
/// chatty child can never block on a full pipe.
pub fn output_within(
    cmd: &mut std::process::Command,
    stdin: Option<&[u8]>,
    limit: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_t = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_t = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A write error (the child exited early) surfaces as its exit status.
        let _ = pipe.write_all(bytes);
        drop(pipe);
    }
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_t.join();
            let _ = err_t.join();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "timed out after {} s — the host accepted the connection but did not finish",
                    limit.as_secs()
                ),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    Ok(std::process::Output {
        status,
        stdout: out_t.join().unwrap_or_default(),
        stderr: err_t.join().unwrap_or_default(),
    })
}

#[cfg(all(test, unix))]
mod output_within_tests {
    use super::*;

    #[test]
    fn a_wedged_child_is_killed_at_the_limit() {
        // B1-08: an ssh session that never finishes must not hang the caller.
        let started = std::time::Instant::now();
        let err = output_within(
            std::process::Command::new("sleep").arg("30"),
            None,
            std::time::Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn output_and_stdin_round_trip() {
        let out = output_within(
            &mut std::process::Command::new("cat"),
            Some(b"hello"),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hello");
    }
}

/// Clear `HANDLE_FLAG_INHERIT` on this process's standard handles (review
/// B2-06). Called first thing on the `--mcp-stdio` path; a no-op elsewhere.
#[cfg(windows)]
pub fn clear_std_handle_inheritance() {
    use windows_sys::Win32::Foundation::{
        SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle has no preconditions; SetHandleInformation is
        // called only on a handle GetStdHandle returned as valid.
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Unix: `spawn_detached` `dup2`s over fds 0/1/2, which drops the originals
/// for real, so there is nothing to clear.
#[cfg(not(windows))]
pub fn clear_std_handle_inheritance() {}
