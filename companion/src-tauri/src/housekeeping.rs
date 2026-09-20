//! Kill stale `aiui --mcp-stdio` children left over from older app versions.
//!
//! Context: an MCP host (Claude Desktop, Claude Code, Cowork, Codex) spawns
//! `aiui --mcp-stdio` once and keeps it alive for the whole session. If the
//! user updates the aiui binary while a session is live, those already-spawned
//! children keep running with the *old* code. Their lifetime-channel logic may
//! be pre-auto-resurrect (≤ v0.2.5) or otherwise incompatible, so the user ends
//! up with a stale MCP server that refuses to reconnect to the new GUI.
//!
//! # One orphan predicate for all three sweeps (#200)
//!
//! Three sweeps in this module ask the same question — *"is this process mine
//! and genuinely abandoned?"* — and all three now answer it with the single
//! `is_orphaned_child` predicate. That predicate is cross-platform **by
//! construction**: `ppid == 1` covers macOS/Linux, where the kernel reparents
//! an orphan to launchd/init; `!snap.contains(ppid)` covers Windows, which does
//! not reparent — a dead parent's pid simply stops appearing in the snapshot.
//! Nothing outside `is_orphaned_child` may test `ppid == 1` again.
//!
//! Why the gate is load-bearing: only Claude Desktop respawns an MCP child it
//! loses. Claude Code, Cowork and Codex do **not** — killing a live child there
//! is a one-way trip to `Server disconnected` on the user's next tool call.
//! This was fixed twice before (v0.4.46 Bug A, v0.8.2) for the orphan and
//! pre-GUI sweeps; #200 closes the third instance in the path-based sweep.
//!
//! # The three process sweeps
//!
//!  1. **GUI-side path sweep** (`kill_stale_mcp_stdio_children`): on every GUI
//!     startup we scan for *orphaned* `aiui --mcp-stdio` processes whose
//!     executable path differs from ours and signal them to terminate. Paths
//!     are compared canonicalized, and a process whose `exe()` sysinfo could
//!     not read (so the snapshot fell back to `argv[0]`) is never classified
//!     stale — we don't kill on evidence we couldn't read.
//!
//!  2. **Pre-GUI sweep** (`kill_mcp_stdio_started_before_self`): orphaned
//!     mcp-stdio children older than this GUI generation.
//!
//!  3. **Orphan tunnel sweep** (`kill_aiui_ssh_ntr` with `only_orphans`):
//!     `ssh -N -T -R <port>:localhost:<port>` children whose aiui parent died
//!     without firing `kill_on_drop`. Orphan-gated by `is_orphaned_child`
//!     since #200 — the previous `ppid == Some(1)` literal made this a
//!     permanent no-op on Windows, where a force-quit `aiui.exe` left the
//!     tunnel holding the remote's port across every restart.
//!
//! # Subprocess-side self-check (the in-place-update path)
//!
//! A sweep can only see a path change. When the user replaces the binary *in
//! place* the path is identical, so the child has to notice by itself and exit
//! cleanly — the one shutdown every host does honour with a respawn. Two
//! checks run at `--mcp-stdio` start and then every 30 s (`lib.rs`):
//!
//!  * `disk_version_if_stale` (macOS): reads `CFBundleShortVersionString` from
//!    the on-disk `Info.plist` two directories up from `argv[0]` and compares
//!    it with the compile-time `CARGO_PKG_VERSION`.
//!  * `current_exe_mtime` / `is_exe_mtime_stale` (all platforms): records the
//!    mtime of `current_exe()` at start and exits when it changes. This is the
//!    Windows analogue of the version check — there is no `Info.plist` there,
//!    and after #200 orphan-gated the path sweep it is what keeps a Windows
//!    NSIS in-place update from leaving a live child on pre-update code.
//!
//! Cross-platform via `sysinfo`: every sweep enumerates processes with the
//! same API, no `ps`/`tasklist` shell-out, no /proc assumption.
//!
//! Safety: we never kill our own pid. If the current binary path can't be
//! determined, we skip the path-based sweep entirely. Every kill goes through
//! `terminate_victim`, which re-asserts the victim's identity (exe leaf + argv)
//! against the sweep's own snapshot before it signals, so a pid recycled
//! between "proved it" and "signal it" is refused rather than killed.
//!
//! Idempotent: running on a clean system is a no-op.

use crate::logging::trace;
use fs4::fs_std::FileExt;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use sysinfo::{ProcessRefreshKind, RefreshKind, Signal, System, UpdateKind};

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
        let mut opts = OpenOptions::new();
        opts.create(true).read(true).write(true).truncate(false);
        // Issue #185: the lock file must not be world-readable/writable.
        // `mode()` applies only on creation, so an install that already
        // carries a 0644 lock is repaired right after a successful acquire.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&path)?;
        file.try_lock_exclusive()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(md) = std::fs::metadata(&path) {
                if md.permissions().mode() & 0o177 != 0 {
                    let _ = std::fs::set_permissions(
                        &path,
                        std::fs::Permissions::from_mode(0o600),
                    );
                }
            }
        }
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

/// Windows' `ERROR_LOCK_VIOLATION`: what `LockFileEx` reports with
/// `LOCKFILE_FAIL_IMMEDIATELY` when someone else holds the range. `std`
/// leaves it uncategorised, so the raw code is the only reliable signal
/// there — on Unix the same situation arrives as `EWOULDBLOCK`/`EAGAIN`,
/// which `std` does map to [`io::ErrorKind::WouldBlock`].
#[cfg(windows)]
const ERROR_LOCK_VIOLATION: i32 = 33;

/// Does this `try_acquire` error mean "another process holds the lock"?
///
/// #196: [`ProcessLock::try_acquire`] fails for two unrelated reasons, and
/// the caller used to collapse both into "another aiui-GUI is running" and
/// exit silently. Contention is the benign one. Everything else — a
/// deny-share handle from a backup/AV agent, `LockFileEx` refusing a
/// network-redirected roaming `%APPDATA%`, a deny-ACL or read-only attribute
/// on `gui.lock`, a leftover `gui.lock` that is a *directory* — is no
/// evidence at all that a second GUI exists, and must not be reported as
/// one.
pub fn is_lock_contention(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(windows)]
    {
        if e.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            return true;
        }
    }
    false
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
    /// Parent PID, exactly as the OS reports it — never interpreted here.
    /// `is_orphaned_child` is the only place that decides what a given ppid
    /// means, because the meaning differs per platform: macOS/Linux reparent
    /// an orphan to launchd/init, Windows leaves the ppid naming a dead
    /// process. `None` only when sysinfo can't resolve the parent at all.
    ppid: Option<u32>,
    exe: String,
    args: Vec<String>,
    /// `false` when sysinfo could not resolve the executable and `exe`
    /// above is the `argv[0]` fallback — possibly relative, possibly a
    /// bare command name, in any case *not* a path we may compare
    /// against `current_exe()`. The path-based stale sweep skips such a
    /// process rather than killing it on evidence it could not read
    /// (#200).
    exe_resolved: bool,
    /// Process start time in seconds since the Unix epoch (whatever
    /// `sysinfo::Process::start_time` returns for the current OS).
    /// Used by the sibling-mcp-stdio sweep to enforce a strict
    /// "younger kills older" rule and avoid two simultaneously-started
    /// duplicates from killing each other. v0.4.42.
    start_time: u64,
}

/// Exactly the process fields the filters in this module read: `cmd` (for
/// `has_mcp_stdio_flag` / `is_aiui_ssh_ntr_for_port`) and `exe` (for
/// `is_aiui_binary` and the path comparison). `ppid` and `start_time` are
/// core fields sysinfo always refreshes, so they need no opt-in.
///
/// Deliberately *not* `ProcessRefreshKind::everything()` (#200): that also
/// pulls cpu, memory, disk_usage, user, cwd, root and environ for every
/// process on the machine, which cost hundreds of milliseconds we then spent
/// widening the window between proving a victim and signalling it.
fn process_fields() -> ProcessRefreshKind {
    ProcessRefreshKind::new()
        .with_cmd(UpdateKind::Always)
        .with_exe(UpdateKind::Always)
}

/// Executable path of `p`, plus whether sysinfo actually resolved it.
/// `false` means the string is the `argv[0]` fallback. Shared by
/// `snapshot_processes` and `terminate_victim` so the identity re-check
/// applies the exact same rule the finder did.
fn resolve_exe(p: &sysinfo::Process) -> (String, bool) {
    match p.exe() {
        Some(e) => (e.to_string_lossy().to_string(), true),
        None => (
            p.cmd()
                .first()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            false,
        ),
    }
}

/// Enumerate every running process via `sysinfo` once and return both the
/// live `System` and a snapshot of it. Cross-platform: identical behaviour
/// on macOS, Linux, and Windows.
///
/// The `System` is returned rather than dropped so the caller can hand it to
/// `terminate_victim`: killing off the *same* enumeration that proved the
/// victim is what closes the pid-recycling window (#200).
fn snapshot_processes() -> (System, Vec<ProcSnap>) {
    let sys =
        System::new_with_specifics(RefreshKind::new().with_processes(process_fields()));
    let snap = sys
        .processes()
        .iter()
        .map(|(pid, p)| {
            let (exe, exe_resolved) = resolve_exe(p);
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
                exe_resolved,
                start_time: p.start_time(),
            }
        })
        .collect();
    (sys, snap)
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

/// Lexically normalize an executable path for comparison: unify the two
/// separators, drop `.` segments, resolve `..`, collapse repeats. Purely
/// textual — it never touches the filesystem, so it still works for a
/// process whose binary has since been moved or replaced.
fn normalize_exe_path(path: &str) -> String {
    let leading: String = path
        .chars()
        .take_while(|c| *c == '/' || *c == '\\')
        .map(|_| '/')
        .collect();
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    format!("{leading}{}", out.join("/"))
}

/// True iff `a` and `b` name the same executable. Compares canonicalized
/// paths when both resolve on disk, and falls back to a lexical comparison
/// otherwise — the binary a running child was started from may already have
/// been moved away by the update we are reacting to.
///
/// #200: the sweep used to compare the two strings raw, so `/Applications/./
/// aiui.app/…` or any other spelling of the install path classified a live
/// child as stale.
fn same_exe_path(a: &str, b: &str) -> bool {
    if a == b || normalize_exe_path(a) == normalize_exe_path(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Filter: *orphaned* `aiui --mcp-stdio` children running from a different
/// executable path than ours, excluding `own_pid`. Pure function over a
/// snapshot, kept testable.
///
/// Orphan-gated since #200, mirroring `find_orphaned_mcp_stdio_to_kill` and
/// `find_pre_gui_mcp_stdio_to_kill`. Without the gate, any GUI start killed
/// every live `--mcp-stdio` child whose path string differed from ours — the
/// user moving `aiui.app` out of `~/Downloads`, a dev build beside the
/// released one, or an update while a session is open. Claude Desktop
/// respawns such a child; Claude Code, Cowork and Codex do not, so the user
/// got `Server disconnected` on their next tool call. The in-place-update
/// case this sweep cannot see is handled child-side — see the module
/// docstring.
///
/// Two further #200 tightenings on the path test itself: paths are compared
/// canonicalized rather than as raw strings, and a process whose `exe` is the
/// `argv[0]` fallback (`exe_resolved == false`) is skipped outright — an
/// unreadable exe is not evidence of staleness.
fn find_stale(snap: &[ProcSnap], current_exe_path: &str, own_pid: u32) -> Vec<StaleChild> {
    snap.iter()
        .filter(|p| p.pid != own_pid)
        .filter(|p| has_mcp_stdio_flag(&p.args))
        .filter(|p| p.exe_resolved)
        .filter(|p| is_aiui_binary(&p.exe))
        .filter(|p| !same_exe_path(&p.exe, current_exe_path))
        .filter(|p| is_orphaned_child(snap, p))
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

/// A process is *orphaned* when its parent is gone. **The** orphan
/// definition for this module: all three sweeps route through it, and no
/// other code may re-derive one (#200).
///
/// Cross-platform by construction — the three arms are not alternatives,
/// they are the three shapes "parent is gone" takes:
///
/// * `None` — sysinfo could not resolve the parent at all.
/// * `Some(1)` — macOS/Linux: the kernel reparents an orphan to
///   launchd/init. Windows never produces this.
/// * `Some(pp)` not in the snapshot — Windows: nothing reparents an orphan,
///   the recorded ppid keeps naming the dead parent, and a dead pid simply
///   stops appearing in the enumeration. Also catches the macOS race where
///   the parent died between fork and our snapshot.
///
/// Only orphaned children are safe to reap: their MCP client (the Claude.app
/// / Cowork helper wrapper that spawned them) has died, so they are genuinely
/// abandoned. A child whose parent is still alive belongs to a *live* session
/// — and must be spared, because only Claude Desktop respawns one it loses.
///
/// Known limit: on Windows a *recycled* parent pid can make a genuine orphan
/// look live. Closing that needs a `start_time` ordering check on the
/// surviving parent, which is a separate change with its own test — not
/// folded in here.
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
    let (sys, snap) = snapshot_processes();
    let victims = find_orphaned_mcp_stdio_to_kill(&snap, own_pid);

    let mut killed = 0usize;
    for victim in &victims {
        trace(&format!(
            "housekeeping: reaping orphaned mcp-stdio pid={} exe={} \
             (parent gone — abandoned leak)",
            victim.pid, victim.exe
        ));
        if terminate_victim(&sys, victim.pid, VictimKind::McpStdio) {
            killed += 1;
        }
    }
    let n = victims.len();
    if n > 0 {
        trace(&format!(
            "housekeeping: reaped {killed} of {n} orphaned mcp-stdio child(ren) on startup"
        ));
    }
    killed
}

/// Filter: every *orphaned* `aiui --mcp-stdio` child started strictly
/// before `own_start_time`, excluding `own_pid`. Pure function over a
/// snapshot — caller passes own pid + own start_time so tests don't
/// need to spoof `std::process::id`.
///
/// Called by the GUI at startup right after it wins the
/// process-lifetime lock. Original intent (v0.4.43): clear out
/// mcp-stdio children left over from a *previous* GUI generation —
/// they carry the pre-update binary in RAM, and `disk_version_if_stale`
/// alone wouldn't reach them.
///
/// Rescoped (v0.8.2): require the child to also be **orphaned**
/// (`is_orphaned_child` — parent process gone). The previous "older
/// than the GUI" rule alone tore down the mcp-stdio child that had
/// just bootstrapped the very GUI which then ran the sweep — Cowork's
/// 2026-06-08 "Server disconnected, then `housekeeping: killing
/// pre-GUI mcp-stdio child pid=2483 … cutoff=1780932439`" trace. The
/// old code's docstring assumed "Claude Desktop respawns them against
/// the current binary"; Claude Code / Cowork **do not** respawn, so
/// the bootstrapper just died and the user saw an MCP error every
/// cold start. Orphan-status is the precise discriminator: a stale
/// leftover from a dead client has lost its parent; a live
/// bootstrapper has not.
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
        .filter(|p| is_orphaned_child(snap, p))
        .map(|p| StaleChild {
            pid: p.pid,
            exe: p.exe.clone(),
        })
        .collect()
}

/// Public entry: terminate every *orphaned* `aiui --mcp-stdio` child
/// older than us. Returns the count of children signalled. Safe to
/// call from the GUI startup path after winning the process-lifetime
/// lock — no race because at most one GUI holds the lock.
///
/// Rescoped in v0.8.2: requires orphan-status (parent gone) so we no
/// longer take down the live bootstrapper child that just spawned us.
/// See `find_pre_gui_mcp_stdio_to_kill` for the rationale.
pub fn kill_mcp_stdio_started_before_self() -> usize {
    let own_pid = std::process::id();
    let (sys, snap) = snapshot_processes();
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
    let mut killed = 0usize;
    for victim in &victims {
        trace(&format!(
            "housekeeping: killing pre-GUI orphan mcp-stdio pid={} exe={} \
             (older than GUI cutoff={} AND parent gone)",
            victim.pid, victim.exe, own_start_time
        ));
        if terminate_victim(&sys, victim.pid, VictimKind::McpStdio) {
            killed += 1;
        }
    }
    if !victims.is_empty() {
        trace(&format!(
            "housekeeping: terminated {killed} of {} pre-GUI orphan mcp-stdio child(ren) at startup",
            victims.len()
        ));
    }
    killed
}

/// What a sweep proved about a process before it decided to signal it.
/// `terminate_victim` re-asserts the matching claim against the live
/// process right before it signals (#200).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VictimKind {
    /// An `aiui --mcp-stdio` child.
    McpStdio,
    /// One of our `ssh -N -T -R <port>:localhost:<port>` tunnel children.
    SshNtr(u16),
}

/// Pure re-verification: does the process currently holding a pid still look
/// like the victim the sweep proved? Kept free of `sysinfo` so it is unit
/// testable without spawning anything.
///
/// "Does the pid still exist" is deliberately *not* the test: pid recycling
/// means *exists* is not *is the same process*. The argv/exe re-assertion is
/// the load-bearing part.
fn victim_identity_matches(kind: VictimKind, exe: &str, args: &[String]) -> bool {
    match kind {
        VictimKind::McpStdio => is_aiui_binary(exe) && has_mcp_stdio_flag(args),
        VictimKind::SshNtr(port) => is_aiui_ssh_ntr_for_port(args, port),
    }
}

/// Terminate one victim out of the enumeration the caller already built, and
/// report whether the kill actually landed.
///
/// Three things this does that the old `terminate_pid` did not (#200):
///
/// * It reuses the caller's `System` instead of running a second full
///   `ProcessRefreshKind::everything()` enumeration *per victim*. Those
///   hundreds of milliseconds were exactly the window in which a victim could
///   exit and the OS hand its pid to someone else.
/// * It re-asserts the victim's identity before signalling, so a pid that no
///   longer carries the argv/exe the sweep matched on is refused, not killed.
/// * It returns the outcome instead of dropping it, so callers can log how
///   many kills landed rather than how many victims they found.
///
/// Signal: `SIGTERM` on Unix. On Windows sysinfo supports only `Signal::Kill`,
/// so `kill_with` returns `None` and the fallback `p.kill()` shells out to
/// `taskkill.exe /PID <pid> /F` — a hard, ungraceful termination. That
/// asymmetry is deliberate (there is no graceful equivalent available here),
/// but it is not "the equivalent terminate-by-handle" the old docstring
/// claimed.
fn terminate_victim(sys: &System, pid: u32, expect: VictimKind) -> bool {
    let Some(p) = sys.process(sysinfo::Pid::from_u32(pid)) else {
        trace(&format!(
            "housekeeping: pid={pid} no longer in the snapshot — nothing signalled"
        ));
        return false;
    };
    let (exe, _) = resolve_exe(p);
    let args: Vec<String> = p
        .cmd()
        .iter()
        .map(|s| s.to_string_lossy().to_string())
        .collect();
    if !victim_identity_matches(expect, &exe, &args) {
        trace(&format!(
            "housekeeping: refusing to signal pid={pid} — identity no longer \
             matches {expect:?} (exe={exe}); pid recycled or process changed"
        ));
        return false;
    }
    match p.kill_with(Signal::Term) {
        Some(true) => true,
        Some(false) => {
            trace(&format!(
                "housekeeping: SIGTERM to pid={pid} failed (permission denied \
                 or already gone)"
            ));
            false
        }
        None => {
            // Windows: Signal::Term is unsupported, so this is
            // `taskkill /PID <pid> /F`.
            let landed = p.kill();
            if !landed {
                trace(&format!(
                    "housekeeping: hard kill of pid={pid} failed (permission \
                     denied or already gone)"
                ));
            }
            landed
        }
    }
}

/// Scan for stale `aiui --mcp-stdio` processes and terminate the ones that
/// are *orphaned* and run from an executable path other than
/// `current_exe_path`. Returns the number of processes actually killed.
///
/// See `find_stale` for why the orphan gate is there (#200) — in short, a
/// path mismatch alone is not abandonment, and the hosts other than Claude
/// Desktop do not respawn a child we take down.
pub fn kill_stale_mcp_stdio_children(current_exe_path: &str) -> usize {
    let own_pid = std::process::id();
    let (sys, snap) = snapshot_processes();
    let stale = find_stale(&snap, current_exe_path, own_pid);

    let mut killed = 0usize;
    for child in &stale {
        trace(&format!(
            "housekeeping: killing stale mcp-stdio child pid={} exe={} \
             (different path AND parent gone)",
            child.pid, child.exe
        ));
        if terminate_victim(&sys, child.pid, VictimKind::McpStdio) {
            killed += 1;
        }
    }

    if !stale.is_empty() {
        trace(&format!(
            "housekeeping: terminated {killed} of {} stale mcp-stdio child(ren)",
            stale.len()
        ));
    }
    killed
}

/// Sibling of `kill_stale_mcp_stdio_children` that doesn't filter by
/// executable path — every running `aiui --mcp-stdio` (other than our
/// own pid) gets terminated. Bound to the uninstall flow (#72): without
/// this, the auto-resurrect loop in `mcp_attach` would relaunch the GUI
/// the moment we call `app.exit(0)`.
pub fn kill_all_mcp_stdio_children() -> usize {
    let own_pid = std::process::id();
    let (sys, snap) = snapshot_processes();
    let children = find_all_children(&snap, own_pid);

    let mut killed = 0usize;
    for child in &children {
        trace(&format!(
            "housekeeping: killing mcp-stdio child pid={} exe={} (uninstall sweep)",
            child.pid, child.exe
        ));
        if terminate_victim(&sys, child.pid, VictimKind::McpStdio) {
            killed += 1;
        }
    }

    if !children.is_empty() {
        trace(&format!(
            "housekeeping: terminated {killed} of {} mcp-stdio child(ren) for uninstall",
            children.len()
        ));
    }
    killed
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

/// Is this tunnel child orphaned — i.e. did the aiui that spawned it die
/// without taking it along?
///
/// The answer is platform-shaped, which is why the rule is a parameter
/// rather than a `cfg!` buried in the filter (it keeps both branches
/// testable from whichever host the suite runs on):
///
///  * `reparents_to_init` (POSIX): a dead parent hands the child to
///    launchd/init, so `ppid == 1` *is* the orphan signal, and a live ppid
///    means a live owner. Deliberately strict — a second aiui must not read
///    the winner's active tunnels as abandoned.
///  * Windows: nothing re-parents. The child keeps pointing at a ppid that
///    simply no longer exists, so the criterion is "parent not in the
///    snapshot" — the same reasoning `is_orphaned_child` already applies to
///    stranded mcp-stdio children.
///
/// #197: the filter used to test `ppid == Some(1)` unconditionally, so on
/// Windows the startup sweep could never reclaim anything. Combined with the
/// updater's `process::exit(0)`, every Windows update leaked an `ssh -NTR`
/// child that the next instance then mistook for a healthy shared forward.
fn ssh_ntr_is_orphan(snap: &[ProcSnap], p: &ProcSnap, reparents_to_init: bool) -> bool {
    if reparents_to_init {
        p.ppid == Some(1)
    } else {
        match p.ppid {
            None => true,
            Some(pp) => !snap.iter().any(|q| q.pid == pp),
        }
    }
}

/// Filter: every `ssh -NTR <port>:localhost:<port>` process matching our
/// tunnel signature. When `only_orphans` is true, restrict to the ones whose
/// spawning aiui is gone — see [`ssh_ntr_is_orphan`] for the per-platform
/// rule — the case where an earlier aiui crashed out of `app.exit()` /
/// `process::exit()` without firing `kill_on_drop`. When false, return all
/// matching processes (used pre-exit so we sweep our own active tunnels
/// before the rust-side Drop is skipped). Pure over a snapshot.
fn find_aiui_ssh_ntr(snap: &[ProcSnap], port: u16, only_orphans: bool) -> Vec<u32> {
    find_aiui_ssh_ntr_with_rule(snap, port, only_orphans, cfg!(unix))
}

/// [`find_aiui_ssh_ntr`] with the orphan rule spelled out, so both platform
/// behaviours can be asserted from a single test host.
fn find_aiui_ssh_ntr_with_rule(
    snap: &[ProcSnap],
    port: u16,
    only_orphans: bool,
    reparents_to_init: bool,
) -> Vec<u32> {
    snap.iter()
        .filter(|p| is_aiui_ssh_ntr_for_port(&p.args, port))
        .filter(|p| !only_orphans || ssh_ntr_is_orphan(snap, p, reparents_to_init))
        .map(|p| p.pid)
        .collect()
}

/// Convenience wrapper for the pre-exit path: traces the imminent exit
/// and sweeps our own active ssh-NTR tunnels so they don't outlive us as
/// launchd-orphans when `app.exit()` / `process::exit()` skip Rust Drop.
///
/// Called immediately before every exit point in the GUI process. v0.4.37.
pub fn pre_exit_cleanup(port: u16, reason: &str) {
    exit_cleanup(port, reason, SweepScope::All);
}

/// How much of the ssh-NTR tunnel fleet an exiting instance may sweep.
///
/// #180: the sweep used to be unconditional, which is wrong for the
/// multi-instance exits. A second aiui that loses the startup race never
/// opened a tunnel — every `ssh -N -T -R` on the machine belongs to the
/// instance that *won*. Sweeping "all" on its way out therefore killed the
/// live instance's tunnels, dropping every remote session's dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepScope {
    /// This instance owned the tunnels: take them with us.
    All,
    /// Take only tunnels whose parent is gone.
    OrphansOnly,
    /// Another instance owns them. Touch nothing.
    None,
}

pub fn exit_cleanup(port: u16, reason: &str, scope: SweepScope) {
    match scope {
        SweepScope::None => {
            trace(&format!(
                "[aiui] exit ({reason}): another instance owns the ssh-NTR tunnels — not sweeping"
            ));
        }
        SweepScope::OrphansOnly | SweepScope::All => {
            let only_orphans = scope == SweepScope::OrphansOnly;
            let mode = if only_orphans { "orphan" } else { "all" };
            trace(&format!(
                "[aiui] exit ({reason}): cleaning up {mode} ssh-NTR tunnels before shutdown"
            ));
            let killed = kill_aiui_ssh_ntr(port, only_orphans);
            trace(&format!(
                "[aiui] exit ({reason}): swept {killed} ssh-NTR child(ren); proceeding"
            ));
        }
    }
}

/// Sweep ssh-NTR tunnel children — see `find_aiui_ssh_ntr` for the filter.
/// Returns the number of kills that actually landed (not the number of
/// matches found). Logs each kill to the trace for post-mortem
/// debuggability of the v0.4.36 orphan-tunnel-loop.
pub fn kill_aiui_ssh_ntr(port: u16, only_orphans: bool) -> usize {
    let (sys, snap) = snapshot_processes();
    let pids = find_aiui_ssh_ntr(&snap, port, only_orphans);
    let mode = if only_orphans { "orphan" } else { "all" };
    let mut killed = 0usize;
    for pid in &pids {
        trace(&format!(
            "housekeeping: killing {mode} ssh-NTR tunnel pid={pid}"
        ));
        if terminate_victim(&sys, *pid, VictimKind::SshNtr(port)) {
            killed += 1;
        }
    }
    if !pids.is_empty() {
        trace(&format!(
            "housekeeping: terminated {killed} of {} {mode} ssh-NTR tunnel(s) on :{port}",
            pids.len()
        ));
    }
    killed
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

/// Modification time of our own on-disk executable, in seconds since the
/// Unix epoch. `None` when `current_exe()` is unresolvable, the file is gone
/// (an updater may have moved it aside), or the platform reports no mtime.
///
/// The cheap, portable analogue of `disk_version_if_stale` (#200): an mcp-stdio
/// child records this at start and compares it on every periodic tick. It is
/// what keeps a **Windows** in-place NSIS update from leaving a live child on
/// pre-update code, now that the GUI-side path sweep is orphan-gated and no
/// longer force-refreshes a child whose host is still alive. Exiting cleanly
/// is also the only shutdown Claude Code / Cowork / Codex honour with a
/// respawn — the GUI killing the child is not.
pub fn current_exe_mtime() -> Option<u64> {
    let exe = std::env::current_exe().ok()?;
    let mtime = std::fs::metadata(exe).ok()?.modified().ok()?;
    mtime
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Pure decision: has the on-disk executable been replaced since `baseline`
/// was recorded? An unknown value on either side means "we couldn't read it"
/// → not stale, same conservative rule `is_disk_version_stale` applies to an
/// empty version string. Better to keep serving than to abort a working
/// subprocess on a transient stat failure.
pub(crate) fn is_exe_mtime_stale(baseline: Option<u64>, current: Option<u64>) -> bool {
    match (baseline, current) {
        (Some(b), Some(c)) => b != c,
        _ => false,
    }
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
            // pid 0 is never in these fixtures, so every process built by
            // this helper is an orphan under `is_orphaned_child` — which is
            // what the find_stale tests below rely on now that the sweep is
            // orphan-gated (#200).
            ppid: Some(0),
            exe: exe.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            exe_resolved: true,
            start_time: 0,
        }
    }

    fn snap_with_ppid(pid: u32, ppid: u32, exe: &str, args: &[&str]) -> ProcSnap {
        ProcSnap {
            pid,
            ppid: Some(ppid),
            exe: exe.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            exe_resolved: true,
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
            exe_resolved: true,
            start_time,
        }
    }

    /// Like `snap_with_ppid`, but for a process whose `exe()` sysinfo could
    /// not read — `exe` is then the `argv[0]` fallback, not a real path.
    fn snap_unresolved_exe(pid: u32, ppid: u32, exe: &str, args: &[&str]) -> ProcSnap {
        ProcSnap {
            exe_resolved: false,
            ..snap_with_ppid(pid, ppid, exe, args)
        }
    }

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    /// #210: the `aiui.exe` leaf is what keeps the Windows sweep from
    /// signalling a process that merely carries `--mcp-stdio` in its argv.
    /// Platform-independent by construction (both separators are split on),
    /// so both shapes are asserted on every leg rather than only where the
    /// path happens to be native.
    #[test]
    fn is_aiui_binary_matches_the_exe_leaf_on_windows() {
        assert!(is_aiui_binary(r"C:\Program Files\aiui\aiui.exe"));
        assert!(is_aiui_binary(r"C:\Program Files\aiui\AIUI.EXE"));
        assert!(!is_aiui_binary(r"C:\x\notaiui.exe"));
        // The Unix shapes keep working from the same predicate.
        assert!(is_aiui_binary("/Applications/aiui.app/Contents/MacOS/aiui"));
        assert!(!is_aiui_binary("/usr/local/bin/aiui-mcp"));
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

    // ---------- find_stale: orphan gate + path handling (#200) ----------

    /// The same install path as `CURRENT`, spelled with a redundant `.`
    /// segment — what a host that re-derives the path can hand us.
    #[cfg(windows)]
    const CURRENT_EQUIVALENT: &str = r"C:\Program Files\.\aiui\aiui.exe";
    #[cfg(not(windows))]
    const CURRENT_EQUIVALENT: &str = "/Applications/./aiui.app/Contents/MacOS/aiui";

    #[test]
    fn stale_sweep_spares_child_with_live_parent() {
        // The mirror of `pre_gui_kill_spares_bootstrapper_with_live_parent`.
        // The user moved aiui.app out of ~/Downloads, so a live Claude Code /
        // Cowork / Codex session's child now runs from a path that differs
        // from ours. Its wrapper parent (200) is alive → live session →
        // spared. Killing it would hand the user `Server disconnected` on
        // their next tool call, and those hosts never respawn.
        let s = vec![
            snap_full(100, 1, "/Applications/Claude.app/Contents/MacOS/Claude", &["Claude"], 500),
            snap_full(200, 100, "/Applications/Claude.app/Contents/Helpers/disclaimer", &["disclaimer"], 999),
            snap_full(300, 200, "/Users/me/Downloads/aiui.app/Contents/MacOS/aiui", &["aiui", "--mcp-stdio"], 1000),
        ];
        assert!(
            find_stale(&s, CURRENT, 1).is_empty(),
            "live child at an old path must be spared (#200)"
        );
    }

    #[test]
    fn stale_sweep_reaps_orphaned_old_path_child() {
        // Same child, parent gone — the leak the sweep exists for. Must
        // still be reaped, so the orphan gate narrows the sweep rather than
        // disabling it.
        let s = vec![snap_full(
            300,
            200,
            "/Users/me/Downloads/aiui.app/Contents/MacOS/aiui",
            &["aiui", "--mcp-stdio"],
            1000,
        )];
        let stale = find_stale(&s, CURRENT, 1);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].pid, 300);
    }

    #[test]
    fn stale_sweep_ignores_unresolved_exe() {
        // sysinfo could not read this process's exe, so the snapshot fell
        // back to argv[0] — a bare command name that trivially "differs"
        // from our install path. Never classify a process stale on evidence
        // we could not read.
        let s = vec![snap_unresolved_exe(300, 0, "aiui", &["aiui", "--mcp-stdio"])];
        assert!(
            find_stale(&s, CURRENT, 1).is_empty(),
            "a process with an unresolved exe must never be swept"
        );
    }

    #[test]
    fn stale_sweep_treats_equivalent_paths_as_current() {
        // Two spellings of the one install path. Raw string equality called
        // this stale and killed a child of the running build.
        let s = vec![snap(
            300,
            CURRENT_EQUIVALENT,
            &[CURRENT_EQUIVALENT, "--mcp-stdio"],
        )];
        assert!(
            find_stale(&s, CURRENT, 1).is_empty(),
            "a differently-spelled path to the current binary is not stale"
        );
        assert!(same_exe_path(CURRENT, CURRENT_EQUIVALENT));
        assert!(!same_exe_path(CURRENT, "/old/path/aiui"));
    }

    // ---------- victim identity re-check before signalling (#200) ----------

    #[test]
    fn victim_identity_recheck_rejects_changed_process() {
        let mcp: Vec<String> = [CURRENT, "--mcp-stdio"].iter().map(|s| s.to_string()).collect();
        // Unchanged → still our victim.
        assert!(victim_identity_matches(VictimKind::McpStdio, CURRENT, &mcp));

        // Pid recycled by something that is not aiui.
        assert!(!victim_identity_matches(
            VictimKind::McpStdio,
            "/usr/bin/python3",
            &mcp
        ));
        // Same binary, but no longer an mcp-stdio child — this is the GUI.
        let gui: Vec<String> = [CURRENT].iter().map(|s| s.to_string()).collect();
        assert!(!victim_identity_matches(VictimKind::McpStdio, CURRENT, &gui));

        // Tunnel victims re-assert the port-specific argv shape.
        let tunnel = ssh_ntr_args("dev@devhost", 7777);
        assert!(victim_identity_matches(
            VictimKind::SshNtr(7777),
            "/usr/bin/ssh",
            &tunnel
        ));
        assert!(!victim_identity_matches(
            VictimKind::SshNtr(8888),
            "/usr/bin/ssh",
            &tunnel
        ));
        assert!(!victim_identity_matches(
            VictimKind::SshNtr(7777),
            "/usr/bin/ssh",
            &gui
        ));
    }

    // ---------- cross-platform stale-binary self-check (#200) ----------

    #[test]
    fn exe_mtime_check_treats_change_as_stale() {
        assert!(is_exe_mtime_stale(Some(1_700_000_000), Some(1_700_000_001)));
        assert!(is_exe_mtime_stale(Some(1_700_000_001), Some(1_700_000_000)));
    }

    #[test]
    fn exe_mtime_check_treats_unchanged_and_unknown_as_fresh() {
        assert!(!is_exe_mtime_stale(Some(1_700_000_000), Some(1_700_000_000)));
        // Either side unreadable → keep serving, same rule as an empty
        // on-disk version string.
        assert!(!is_exe_mtime_stale(None, Some(1_700_000_000)));
        assert!(!is_exe_mtime_stale(Some(1_700_000_000), None));
        assert!(!is_exe_mtime_stale(None, None));
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
        let a_orphan = ssh_ntr_args("dev@devhost", 7777);
        let a_live = ssh_ntr_args("customer@macmini", 7777);
        let s = vec![
            // The live GUI that owns the active tunnel below. #200: it has
            // to be *in* the snapshot, otherwise its child is — correctly —
            // an orphan under `is_orphaned_child` and the sweep takes it.
            snap_with_ppid(76770, 1, CURRENT, &[CURRENT]),
            // Orphan from a crashed earlier aiui — re-parented to pid 1.
            snap_with_ppid(30295, 1, "/usr/bin/ssh", &strs(&a_orphan)),
            // Active tunnel from the current GUI (parent 76770 alive).
            snap_with_ppid(40000, 76770, "/usr/bin/ssh", &strs(&a_live)),
        ];
        // Pinned to the POSIX rule explicitly (#197): the shared entry point
        // now picks the rule per platform, and this assertion is about the
        // re-parenting one.
        let orphans = find_aiui_ssh_ntr_with_rule(&s, 7777, true, true);
        assert_eq!(orphans, vec![30295]);

        let all = find_aiui_ssh_ntr_with_rule(&s, 7777, false, true);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn find_aiui_ssh_ntr_orphans_on_windows_ignores_ppid() {
        // #197: Windows never re-parents to pid 1, so the POSIX filter
        // matched nothing there and the startup sweep could not reclaim a
        // tunnel leaked by the updater's `process::exit(0)`. Under the
        // Windows rule the discriminator is whether the parent is still in
        // the snapshot — a matching `ssh -NTR` with `ppid != Some(1)` is an
        // orphan exactly when its parent is gone.
        let s = vec![
            // The surviving aiui that owns pid 40000's tunnel.
            snap_with_ppid(76770, 500, CURRENT, &[CURRENT]),
            // Leaked by a previous instance: ppid 9001 is nowhere any more.
            snap_with_ppid(30295, 9001, "ssh.exe", &ssh_ntr_args("dev@devhost", 7777).iter().map(String::as_str).collect::<Vec<_>>()),
            // Live tunnel of the instance above — must be spared.
            snap_with_ppid(40000, 76770, "ssh.exe", &ssh_ntr_args("customer@macmini", 7777).iter().map(String::as_str).collect::<Vec<_>>()),
        ];
        let orphans = find_aiui_ssh_ntr_with_rule(&s, 7777, true, false);
        assert_eq!(
            orphans,
            vec![30295],
            "a ppid that no longer exists is the Windows orphan signal"
        );

        // Same snapshot under the POSIX rule: neither ppid is 1, so the old
        // filter reclaimed nothing at all — the defect, in one assertion.
        assert!(find_aiui_ssh_ntr_with_rule(&s, 7777, true, true).is_empty());

        // `only_orphans: false` stays rule-independent.
        assert_eq!(
            find_aiui_ssh_ntr_with_rule(&s, 7777, false, false).len(),
            2
        );
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
            exe_resolved: true,
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

    #[test]
    fn pre_gui_kill_spares_bootstrapper_with_live_parent() {
        // THE 2026-06-08 regression test. A fresh Cowork session
        // spawned mcp-stdio #300, which then cold-started GUI #400.
        // The GUI's pre-GUI sweep would see #300 as "older than me"
        // and SIGTERM it — taking down Cowork's still-live MCP
        // connection. Orphan-gate fix: #300's parent wrapper #200
        // is alive in the snap → #300 is a live bootstrapper, not a
        // leak → spared.
        let snap = vec![
            snap_full(100, 1, "/Applications/Claude.app/Contents/MacOS/Claude", &["Claude"], 500),
            snap_full(200, 100, "/Applications/Claude.app/Contents/Helpers/disclaimer", &["disclaimer", CURRENT, "--mcp-stdio"], 999),
            // bootstrapper mcp-stdio: older than the GUI, parent (200) alive
            snap_full(300, 200, CURRENT, &[CURRENT, "--mcp-stdio"], 1000),
            // us — the freshly started GUI:
            snap_full(400, 1, "/Applications/aiui.app/Contents/MacOS/aiui", &["aiui"], 2000),
        ];
        let victims = find_pre_gui_mcp_stdio_to_kill(&snap, 400, 2000);
        assert!(
            victims.is_empty(),
            "bootstrapper child with live parent must be spared (Bug 2026-06-08)"
        );
    }

    #[test]
    fn pre_gui_kill_reaps_orphaned_older_child() {
        // The legit case the sweep was added for (v0.4.43): an
        // mcp-stdio left over from a previous, dead Cowork session —
        // parent wrapper gone, child reparented to launchd. Older
        // than us AND orphaned → must still be reaped.
        let snap = vec![
            snap_full(200, 1, CURRENT, &[CURRENT, "--mcp-stdio"], 1000),
            snap_full(400, 1, "/Applications/aiui.app/Contents/MacOS/aiui", &["aiui"], 2000),
        ];
        let victims = find_pre_gui_mcp_stdio_to_kill(&snap, 400, 2000);
        assert_eq!(victims.len(), 1);
        assert_eq!(victims[0].pid, 200);
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
    fn lock_contention_is_classified_as_contention() {
        // The benign case: someone else holds the lock.
        assert!(is_lock_contention(&io::Error::new(
            io::ErrorKind::WouldBlock,
            "already locked"
        )));
        #[cfg(windows)]
        assert!(is_lock_contention(&io::Error::from_raw_os_error(
            ERROR_LOCK_VIOLATION
        )));

        // Everything else is a filesystem problem, not a second GUI.
        // (`ErrorKind::IsADirectory` would say the third case more plainly,
        // but it is newer than this crate's declared MSRV.)
        for e in [
            io::Error::new(io::ErrorKind::PermissionDenied, "deny ACL on gui.lock"),
            io::Error::new(io::ErrorKind::NotFound, "config dir vanished"),
            io::Error::other("gui.lock is a directory"),
        ] {
            assert!(
                !is_lock_contention(&e),
                "{e} must not read as lock contention"
            );
        }
    }

    #[test]
    fn try_acquire_on_a_directory_is_not_contention() {
        // The concrete case the caller used to mislabel: a leftover
        // `gui.lock` that is a directory. Opening it fails, and reporting
        // that as "another aiui-GUI holds the lock" sends the support path
        // after a process that does not exist.
        let dir = std::env::temp_dir().join(format!(
            "aiui-test-lock-isdir-{}",
            std::process::id()
        ));
        let path = dir.join("gui.lock");
        std::fs::create_dir_all(&path).unwrap();

        let err = match ProcessLock::try_acquire(&path) {
            Ok(_) => panic!("acquiring a directory as a lock file must fail"),
            Err(e) => e,
        };
        assert!(
            !is_lock_contention(&err),
            "a directory where the lock file belongs is a filesystem error ({err}), not contention"
        );
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
            exe_resolved: true,
            start_time: 0,
        }];
        assert!(find_aiui_ssh_ntr(&s, 7777, true).is_empty());
        assert!(find_aiui_ssh_ntr(&s, 7777, false).is_empty());
    }
}
