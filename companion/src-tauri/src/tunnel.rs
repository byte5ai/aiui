//! SSH reverse-tunnel manager. Owns one tokio task per remote host, respawns
//! on exit with exponential backoff, and can be cancelled per-host or
//! globally. Status snapshot is exposed to the frontend.
//!
//! **Shared-forward detection.** If our `ssh -NTR` fails with
//! `ExitOnForwardFailure`, port 7777 on the remote is already taken.
//! Historically we flagged this as a hard failure (red dot, "ssh exit 255"),
//! which was misleading when the occupier was a still-living sshd-sess from
//! an earlier aiui session. In that case, the forward works — we just don't
//! own it. We now probe the remote after a failure: if a plain
//! `curl -sS -f -m 3 http://localhost:<port>/ping` over ssh returns `pong`,
//! we mark the tunnel as `ConnectedShared` and poll periodically instead of
//! retrying `-NTR` aggressively. When the probe starts failing (external
//! session died) we drop back into the normal `ssh -NTR` retry loop.

use crate::logging::trace;
use crate::proc_ext::no_window_tokio;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{oneshot, Mutex};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TunnelStatus {
    Connecting,
    Connected,
    /// Port 7777 on the remote is forwarded by a different process (an
    /// earlier aiui session's sshd-sess that outlived its parent, or a
    /// parallel tool doing the same forward). Our own `-NTR` can't bind,
    /// but dialogs reach the Mac anyway. Periodically re-probed.
    ConnectedShared,
    Failed {
        reason: String,
    },
    Stopped,
}

struct TunnelEntry {
    cancel: Option<oneshot::Sender<()>>,
    status: Arc<Mutex<TunnelStatus>>,
}

pub struct TunnelManager {
    entries: Mutex<HashMap<String, TunnelEntry>>,
    port: u16,
}

impl TunnelManager {
    pub fn new(port: u16) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            port,
        })
    }

    pub async fn ensure(self: &Arc<Self>, host: String) {
        // Refuse to even start a tunnel task for a host alias that
        // would be misinterpreted as an ssh option (defense in depth —
        // `add_remote` validates at the API boundary, this catches
        // anything that slips in through an old `remotes.json`).
        if !crate::setup::is_valid_host_alias(&host) {
            trace(&format!(
                "tunnel[{host}]: refusing to start tunnel — host alias rejected by validator"
            ));
            return;
        }
        let mut entries = self.entries.lock().await;
        if entries.contains_key(&host) {
            return;
        }
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let status = Arc::new(Mutex::new(TunnelStatus::Connecting));
        entries.insert(
            host.clone(),
            TunnelEntry {
                cancel: Some(cancel_tx),
                status: status.clone(),
            },
        );
        drop(entries);

        let port = self.port;
        tokio::spawn(async move {
            run_tunnel(host, port, cancel_rx, status).await;
        });
    }

    pub async fn stop(&self, host: &str) {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.remove(host) {
            if let Some(cancel) = entry.cancel {
                let _ = cancel.send(());
            }
        }
    }

    pub async fn stop_all(&self) {
        let mut entries = self.entries.lock().await;
        for (_, entry) in entries.drain() {
            if let Some(cancel) = entry.cancel {
                let _ = cancel.send(());
            }
        }
    }

    pub async fn snapshot(&self) -> HashMap<String, TunnelStatus> {
        let entries = self.entries.lock().await;
        let mut out = HashMap::new();
        for (k, v) in entries.iter() {
            out.insert(k.clone(), v.status.lock().await.clone());
        }
        out
    }
}

/// Interval at which we re-probe a shared-forward remote to notice when the
/// external occupier dies. Chosen to be fast enough that users don't stare
/// at a stale "connected (shared)" label for long, but slow enough that we
/// aren't ssh-ing per second.
const SHARED_FORWARD_POLL_SECS: u64 = 30;

/// Ask the remote whether localhost:`port` is already answering `/probe`
/// from *our own* aiui. Returns:
///
/// - `Some(true)` only when the responder is identifiably *us* — same
///   pid + same compile-time `build_sha`. That's the only legitimate
///   shared-forward case (stale-sshd-sess that's still tunneling back
///   to our process).
/// - `Some(false)` for any other outcome a tunnel-manager can act on
///   immediately:
///   * port empty / curl 4xx (no aiui at all)
///   * a *different* aiui instance answered (different pid or sha) —
///     this is the multi-instance race; we mustn't accept it as
///     "shared", or the second companion's tool calls would silently
///     route to the first
///   * some other authed-but-unrelated service answered
/// - `None` when ssh itself failed to connect at all — different
///   failure mode than "port unreachable", so the retry loop stays
///   patient instead of flipping state on every blip.
///
/// Token source: `~/.config/aiui/token` on the *remote* host (scp'd
/// there at remote-registration time). If the file is missing on the
/// remote, the probe is treated as inconclusive — we don't have enough
/// to decide.
/// The shell command the probe runs on the remote.
///
/// #187: the token is fed to curl over STDIN, not as an argument. It used
/// to be interpolated into `-H "Authorization: Bearer $T"`, which the
/// remote shell expanded before exec — so the live API token sat in curl's
/// argv, visible in `ps` to every user on that host, once per poll (every
/// 30 s in shared-forward mode). Whoever read it could render dialogs on
/// the user's desktop through the tunnel. `curl -H @-` reads headers from
/// stdin, so the secret never becomes an argv element of any process.
///
/// The heredoc delimiter is deliberately UNQUOTED so the remote shell
/// expands `$T`. Not `printf … | curl -H @-`: where printf is an external
/// binary rather than a builtin, that just moves the token into *its* argv.
/// Not the environment either — `/proc/<pid>/environ` is readable by the
/// same set of users as `cmdline`.
///
/// `-f` makes curl fail on 4xx/5xx (so a 401 reads as not-shared) and
/// `-m 3` caps its time. The missing-token case gets its own exit code so
/// the classifier can tell it apart from a real answer.
///
/// A function rather than an inline `format!` so the tests exercise the
/// string that actually ships. A malformed heredoc would break the probe
/// on every remote at once, and Rust's line continuations make the layout
/// easy to get wrong — a test against a re-typed copy would stay green
/// through exactly that mistake.
fn probe_command(port: u16) -> String {
    let url = format!("http://localhost:{port}/probe");
    format!(
        "T=$(cat ~/.config/aiui/token 2>/dev/null); \
         [ -n \"$T\" ] || exit {NO_TOKEN_EXIT}; \
         curl -sS -f -m 3 -H @- {url} <<AIUI_HDR\n\
         Authorization: Bearer $T\n\
         AIUI_HDR\n"
    )
}

async fn probe_remote_shared_forward(host: &str, port: u16) -> Option<bool> {
    let cmd = probe_command(port);
    let fut = no_window_tokio(
        Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "--",
                host,
                &cmd,
            ])
            // #187: without this, the timeout below drops the future and
            // orphans the ssh child — which keeps holding the remote
            // connection we just gave up on.
            .kill_on_drop(true),
    )
    .output();

    // #187: `ConnectTimeout=5` bounds the TCP connect only. Authentication,
    // a wedged remote shell or a stalled `curl -m 3` are all unbounded after
    // that, and this future is awaited inside the poll loop — one hung probe
    // used to park the whole tunnel task forever.
    let out = match tokio::time::timeout(PROBE_TIMEOUT, fut).await {
        Ok(r) => r,
        Err(_) => {
            trace(&format!(
                "tunnel: shared-forward probe on {host} exceeded {}s — inconclusive",
                PROBE_TIMEOUT.as_secs()
            ));
            return None;
        }
    };

    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let verdict = classify_probe_exit(o.status.code(), &stdout);
            if verdict.is_none() {
                // #187: the `2>/dev/null` that used to swallow this made
                // `-S` pointless and threw away the one line explaining an
                // inconclusive probe.
                let stderr = String::from_utf8_lossy(&o.stderr);
                trace(&format!(
                    "tunnel: shared-forward probe on {host} inconclusive (exit {:?}): {}",
                    o.status.code(),
                    stderr.trim()
                ));
            }
            verdict
        }
        Err(e) => {
            trace(&format!(
                "tunnel: shared-forward probe on {host} could not run ssh: {e}"
            ));
            None
        }
    }
}

/// Overall cap on one shared-forward probe (#187). `ConnectTimeout=5` bounds
/// only the TCP connect; everything after it — authentication, a wedged
/// remote shell, a stalled curl — was unbounded.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Exit status the remote probe script uses to say "no usable token here"
/// (#187). Outside curl's own range, and not 126/127 (shell errors) or 255
/// (ssh transport failure), so it cannot be confused with any of them.
const NO_TOKEN_EXIT: i32 = 111;

/// Turn the remote command's exit status into a probe verdict.
///
/// `Some(true)`/`Some(false)` are claims the tunnel manager acts on
/// immediately; `None` means "inconclusive, ask again later". #187: several
/// outcomes that prove nothing were being reported as `Some(false)` — a
/// missing token, a curl that timed out, a remote without curl at all, an
/// ssh killed by a signal. In `ConnectedShared` mode a wrong `Some(false)`
/// costs a retry storm against a port that is still occupied; `None` costs
/// one extra 30 s poll. So when in doubt, `None`.
///
/// Pure, so the table below is unit-testable without ssh.
fn classify_probe_exit(code: Option<i32>, stdout: &str) -> Option<bool> {
    match code {
        // curl got a 2xx — the body decides whether it is us.
        Some(0) => Some(probe_response_is_self(stdout)),
        // Our own marker: no token on the remote, so we cannot even ask.
        Some(NO_TOKEN_EXIT) => None,
        // curl: 7 = connection refused (nothing listening), 22 = HTTP error
        // under -f (401, or a foreign service). Both are real answers.
        Some(7) | Some(22) => Some(false),
        // curl -m 3 timed out: something listens but did not answer. A
        // wedged responder is not proof the forward is gone.
        Some(28) => None,
        // Shell could not run curl at all (not found / not executable).
        Some(126) | Some(127) => None,
        // ssh transport failure.
        Some(255) => None,
        // Any other curl exit is an error we cannot interpret; and `None`
        // means ssh was killed by a signal.
        _ => None,
    }
}

/// Collapse captured ssh stderr into one short fragment for
/// `TunnelStatus::Failed.reason` (#187).
///
/// Every tunnel failure used to surface as "ssh exit code 255" — the least
/// informative thing ssh can say, and the one users hit most. ssh already
/// explains itself on stderr ("remote port forwarding failed for listen
/// port 7777", "Permission denied (publickey)"), but stderr was piped to
/// /dev/null.
///
/// Kept single-line and truncated on purpose: Settings renders the reason
/// into a one-line `.tunnel-status` div, so a multi-line value breaks the
/// layout. Pure, so the shaping is unit-testable.
fn tail_for_reason(raw: &str, max_lines: usize, max_bytes: usize) -> String {
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(max_lines);
    let mut joined = lines[start..].join("; ");
    if joined.chars().count() > max_bytes {
        joined = joined.chars().take(max_bytes.saturating_sub(1)).collect();
        joined.push('…');
    }
    joined
}

/// How long an ssh child must survive before the link counts as having
/// worked (#187). Matches the optimistic-connect threshold, so anything we
/// were willing to call "connected" also resets the backoff.
const BACKOFF_RESET_UPTIME: Duration = Duration::from_secs(30);

/// Next reconnect delay.
///
/// #187: the backoff only ever doubled, never reset. A link that connects,
/// works for hours and then drops inherited whatever the last startup
/// stumble had left behind — so a flapping connection degraded into a
/// permanent 30 s hole after a handful of drops, during which every remote
/// dialog fails. `uptime` past [`BACKOFF_RESET_UPTIME`] means the link
/// worked; start over.
///
/// Deterministic, so it stays unit-testable — jitter is applied at the
/// sleep site.
fn next_backoff(prev: u64, uptime: Duration) -> u64 {
    if uptime > BACKOFF_RESET_UPTIME {
        1
    } else {
        (prev * 2).min(30)
    }
}

/// Apply ±20 % jitter to a backoff, so several tunnels that dropped
/// together do not retry in lockstep (#187). Impure by nature — kept
/// separate from [`next_backoff`] so the schedule itself stays testable.
fn jittered_secs(secs: u64) -> Duration {
    use rand::Rng;
    let base = secs as f64;
    let factor = rand::thread_rng().gen_range(0.8..=1.2);
    Duration::from_millis((base * factor * 1000.0) as u64)
}

/// Pure decision: given the raw `/probe` response body, decide whether
/// it came from *this* aiui instance (same pid + same build SHA). Any
/// other valid aiui response (different pid, different sha, or an old
/// version that doesn't ship pid/sha at all) is treated as foreign —
/// the caller will not switch to shared-forward poll mode for it.
///
/// Pulled out as a pure function so it can be unit-tested without
/// running ssh.
fn probe_response_is_self(body: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    if !parsed.get("aiui").and_then(|v| v.as_bool()).unwrap_or(false) {
        return false;
    }
    let body_pid = parsed.get("pid").and_then(|v| v.as_u64());
    let body_sha = parsed.get("build_sha").and_then(|v| v.as_str());
    let our_pid = std::process::id() as u64;
    let our_sha = env!("AIUI_GIT_SHA");
    matches!(body_pid, Some(p) if p == our_pid)
        && matches!(body_sha, Some(s) if s == our_sha)
}

/// The full argv of the reverse-tunnel spawn, **including `"ssh"` at index
/// 0** — `housekeeping::is_aiui_ssh_ntr_for_port` matches on `args[0]`, so
/// the program name is part of the shape it recognises.
///
/// Built once here and shared by the spawner (`run_tunnel`, which skips the
/// argv[0] when handing the rest to `Command::new("ssh")`) and the orphan
/// reaper's matcher tests. Before this existed, `housekeeping`'s tests
/// asserted against a hand-copied duplicate of these flags: adding or
/// reordering one `-o` here left the test green while the matcher stopped
/// recognising aiui's own reparented tunnels, so `ssh -NTR` orphans kept
/// piling up on the remote and squatting :7777 (#211).
pub(crate) fn ssh_ntr_args(host: &str, port: u16) -> Vec<String> {
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

async fn run_tunnel(
    host: String,
    port: u16,
    cancel: oneshot::Receiver<()>,
    status: Arc<Mutex<TunnelStatus>>,
) {
    let mut backoff_secs = 1u64;
    let mut cancel_pin = Box::pin(cancel);

    loop {
        trace(&format!(
            "tunnel[{host}]: ssh -NTR {port}:localhost:{port} {host}"
        ));
        *status.lock().await = TunnelStatus::Connecting;

        let argv = ssh_ntr_args(&host, port);
        let mut child = match no_window_tokio(
            Command::new("ssh")
                // `argv[0]` is the program itself, which `Command::new`
                // already supplies.
                .args(argv.iter().skip(1))
                .stdout(std::process::Stdio::null())
                // #187: captured, not discarded. ssh explains its failures
                // here; without it every one surfaced as "ssh exit code 255".
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true),
        )
        .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let err = format!("spawn ssh failed: {e}");
                trace(&format!("tunnel[{host}]: {err}"));
                *status.lock().await = TunnelStatus::Failed { reason: err };
                tokio::select! {
                    _ = tokio::time::sleep(jittered_secs(backoff_secs)) => {}
                    _ = &mut cancel_pin => {
                        *status.lock().await = TunnelStatus::Stopped;
                        return;
                    }
                }
                // No link ever existed here, so there is nothing to reset:
                // plain doubling, same jitter as the other sleep site.
                backoff_secs = next_backoff(backoff_secs, Duration::ZERO);
                continue;
            }
        };

        // Drain stderr into a bounded ring so a chatty motd or `ssh -v`
        // cannot grow it without limit. Taken before `wait()`, because the
        // pipe must be read while the child lives.
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(mut err) = child.stderr.take() {
            let sink = stderr_tail.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 1024];
                loop {
                    match err.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut guard = sink.lock().await;
                            guard.push_str(&String::from_utf8_lossy(&buf[..n]));
                            // Keep only the tail: 4 KB is far more than the
                            // 4 lines we ever render, and bounds the buffer.
                            if guard.len() > 4096 {
                                let cut = guard.len() - 4096;
                                *guard = guard[cut..].to_string();
                            }
                        }
                    }
                }
            });
        }

        // #187: how long the link lived decides whether the backoff resets.
        let started = std::time::Instant::now();

        // Optimistic "connected" after 2s of process survival.
        let status_probe = status.clone();
        let host_probe = host.clone();
        let probe = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            *status_probe.lock().await = TunnelStatus::Connected;
            trace(&format!("tunnel[{host_probe}]: assumed connected"));
        });

        tokio::select! {
            wait_res = child.wait() => {
                probe.abort();
                let base = match wait_res {
                    Ok(s) => s
                        .code()
                        .map(|c| format!("ssh exit code {c}"))
                        .unwrap_or_else(|| "ssh killed by signal".to_string()),
                    Err(e) => format!("wait error: {e}"),
                };
                // #187: say WHY, not just that it died. ssh's own last words
                // ("remote port forwarding failed for listen port 7777") are
                // the difference between a user who can act and one who
                // cannot.
                let tail = tail_for_reason(&stderr_tail.lock().await.clone(), 4, 200);
                let msg = if tail.is_empty() {
                    base
                } else {
                    format!("{base} — {tail}")
                };
                trace(&format!("tunnel[{host}]: ssh died: {msg}"));

                // Before falling into the backoff loop, check if the remote
                // actually has our port forwarded by somebody else. If so,
                // degrade gracefully to shared-forward polling instead of
                // spamming `ssh -NTR` that's guaranteed to keep failing
                // while the zombie session holds the port.
                // #187: cancellable. A `remove_remote` during this probe
                // used to be ignored until it finished.
                let shared = tokio::select! {
                    v = probe_remote_shared_forward(&host, port) => v,
                    _ = &mut cancel_pin => {
                        *status.lock().await = TunnelStatus::Stopped;
                        return;
                    }
                };
                if let Some(true) = shared {
                    trace(&format!(
                        "tunnel[{host}]: shared forward detected — switching to poll mode"
                    ));
                    *status.lock().await = TunnelStatus::ConnectedShared;
                    match shared_forward_poll_loop(&host, port, &status, &mut cancel_pin).await {
                        PollOutcome::LostShare => {
                            // External forward disappeared; drop back to the
                            // normal `-NTR` retry path with a short backoff.
                            backoff_secs = 1;
                            continue;
                        }
                        PollOutcome::Cancelled => {
                            *status.lock().await = TunnelStatus::Stopped;
                            return;
                        }
                    }
                }

                *status.lock().await = TunnelStatus::Failed { reason: msg };
                // #187: a link that worked resets the backoff, so a flapping
                // connection cannot degrade into a permanent 30 s hole.
                // Jitter only at the sleep site, so next_backoff stays
                // deterministic and testable.
                backoff_secs = next_backoff(backoff_secs, started.elapsed());
                let jittered = jittered_secs(backoff_secs);
                tokio::select! {
                    _ = tokio::time::sleep(jittered) => {}
                    _ = &mut cancel_pin => {
                        *status.lock().await = TunnelStatus::Stopped;
                        return;
                    }
                }
            }
            _ = &mut cancel_pin => {
                probe.abort();
                trace(&format!("tunnel[{host}]: cancelled, killing ssh"));
                let _ = child.kill().await;
                *status.lock().await = TunnelStatus::Stopped;
                return;
            }
        }
    }
}

enum PollOutcome {
    /// External forward is no longer answering — caller should resume the
    /// normal `ssh -NTR` retry loop.
    LostShare,
    /// User asked for teardown.
    Cancelled,
}

/// While a shared forward is active, periodically re-probe the remote to
/// confirm it still answers. Stays in `ConnectedShared` as long as the probe
/// succeeds. Returns on first probe failure (`LostShare`) or on cancellation.
async fn shared_forward_poll_loop(
    host: &str,
    port: u16,
    status: &Arc<Mutex<TunnelStatus>>,
    cancel_pin: &mut std::pin::Pin<Box<oneshot::Receiver<()>>>,
) -> PollOutcome {
    loop {
        // Wait out the poll interval — cancellable.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(SHARED_FORWARD_POLL_SECS)) => {}
            _ = &mut *cancel_pin => {
                return PollOutcome::Cancelled;
            }
        }
        // #187: race the probe against cancellation too. It used to sit
        // inside the sleep branch, so a `remove_remote` arriving while the
        // probe was in flight waited for it — and before the probe had its
        // own timeout, that could be forever.
        let verdict = tokio::select! {
            v = probe_remote_shared_forward(host, port) => v,
            _ = &mut *cancel_pin => {
                return PollOutcome::Cancelled;
            }
        };
        match verdict {
            Some(true) => continue,
            Some(false) => {
                trace(&format!(
                    "tunnel[{host}]: shared forward gone — will re-attempt ssh -NTR"
                ));
                return PollOutcome::LostShare;
            }
            None => {
                // Inconclusive. Keep the label as shared; the next poll
                // retries. If it really is gone, a later probe says so.
                trace(&format!(
                    "tunnel[{host}]: shared-forward probe inconclusive, keeping state"
                ));
                let _ = status;
            }
        }
    }
}

#[cfg(test)]
mod probe_cmd_tests {
    use super::*;

    /// The production builder, so these tests check the string that really
    /// ships rather than a re-typed copy that could drift away from it.
    use super::probe_command as probe_cmd;

    #[test]
    fn the_token_is_never_an_argument() {
        // #187 in one assertion: the command may reference the shell
        // variable, but must never place it where argv can be read by
        // another user on the remote host.
        let cmd = probe_cmd(7777);
        assert!(
            !cmd.contains("-H \"Authorization"),
            "no -H with an inline value: {cmd}"
        );
        assert!(cmd.contains("-H @-"), "headers come from stdin: {cmd}");
    }

    #[test]
    fn the_heredoc_is_well_formed() {
        // A malformed heredoc breaks the probe on every remote at once, and
        // Rust's line continuations make the layout easy to get wrong: `\`
        // at end of line eats the newline AND the next line's indentation.
        let cmd = probe_cmd(7777);
        let lines: Vec<&str> = cmd.lines().collect();
        assert_eq!(lines.len(), 3, "three lines exactly: {lines:?}");
        assert!(lines[0].ends_with("<<AIUI_HDR"), "line 0: {:?}", lines[0]);
        assert_eq!(
            lines[1], "Authorization: Bearer $T",
            "the header body must start at column 0, unindented"
        );
        assert_eq!(lines[2], "AIUI_HDR", "the terminator must be alone on its line");
        assert!(
            !cmd.contains("<<'AIUI_HDR'"),
            "the delimiter must be UNQUOTED so the remote shell expands $T"
        );
    }

    #[test]
    fn the_command_is_valid_shell() {
        // Syntax-check with a real shell rather than by eye.
        let out = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(probe_cmd(7777))
            .output()
            .expect("sh is available");
        assert!(
            out.status.success(),
            "sh -n rejected the probe command: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn no_token_exits_with_our_marker() {
        // Run the real command with HOME pointed at an empty dir: no token
        // file, so it must exit with the reserved code rather than running
        // curl and having its failure misread as "the port is free".
        let dir = std::env::temp_dir().join(format!("aiui-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(probe_cmd(7777))
            .env("HOME", &dir)
            .output()
            .expect("sh is available");
        assert_eq!(out.status.code(), Some(NO_TOKEN_EXIT));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_token_file_also_exits_with_the_marker() {
        let dir = std::env::temp_dir().join(format!("aiui-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".config").join("aiui")).unwrap();
        std::fs::write(dir.join(".config").join("aiui").join("token"), "").unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(probe_cmd(7777))
            .env("HOME", &dir)
            .output()
            .expect("sh is available");
        assert_eq!(out.status.code(), Some(NO_TOKEN_EXIT));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backoff_resets_after_a_link_that_worked() {
        // #187: the backoff only ever doubled. A link that connected, ran
        // for hours and then dropped inherited the last startup stumble, so
        // a flapping connection degraded into a permanent 30 s hole during
        // which every remote dialog fails.
        assert_eq!(next_backoff(16, Duration::from_secs(3600)), 1, "worked for an hour");
        assert_eq!(next_backoff(30, Duration::from_secs(31)), 1, "just past the threshold");
        // …but a link that never really came up keeps backing off.
        assert_eq!(next_backoff(1, Duration::from_secs(2)), 2);
        assert_eq!(next_backoff(2, Duration::ZERO), 4);
        assert_eq!(next_backoff(16, Duration::from_secs(29)), 30, "capped");
        assert_eq!(next_backoff(30, Duration::from_secs(1)), 30, "stays capped");
    }

    #[test]
    fn backoff_schedule_is_bounded_and_climbs() {
        // A cold start that never connects: 1,2,4,8,16,30,30…
        let mut b = 1u64;
        let mut seen = vec![b];
        for _ in 0..8 {
            b = next_backoff(b, Duration::ZERO);
            seen.push(b);
        }
        assert_eq!(seen, vec![1, 2, 4, 8, 16, 30, 30, 30, 30]);
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        for secs in [1u64, 5, 30] {
            for _ in 0..200 {
                let d = jittered_secs(secs).as_millis() as f64 / 1000.0;
                assert!(
                    d >= secs as f64 * 0.79 && d <= secs as f64 * 1.21,
                    "{d}s is outside ±20% of {secs}s"
                );
            }
        }
    }

    #[test]
    fn tail_for_reason_keeps_the_useful_last_words() {
        // The failure users hit most, and least understand.
        let raw = "Warning: Permanently added 'devhost' to the list of known hosts.\n\
                   Warning: remote port forwarding failed for listen port 7777\n";
        let out = tail_for_reason(raw, 4, 200);
        assert!(out.contains("remote port forwarding failed for listen port 7777"));
        assert!(!out.contains('\n'), "single line for the Settings row: {out:?}");
    }

    #[test]
    fn tail_for_reason_keeps_only_the_last_lines() {
        // A chatty motd, or `ssh -v`, can produce a lot. Only the tail is
        // rendered — ssh explains itself last.
        let raw = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = tail_for_reason(&raw, 4, 200);
        assert_eq!(out, "line 196; line 197; line 198; line 199");
        assert!(!out.contains('\n'), "single line for the Settings row");
    }

    #[test]
    fn tail_for_reason_truncates_long_output() {
        // Four lines can still overflow the one-line Settings row, so the
        // byte cap applies after the line cap.
        let raw = (0..10)
            .map(|i| format!("line {i} {}", "x".repeat(120)))
            .collect::<Vec<_>>()
            .join("\n");
        let out = tail_for_reason(&raw, 4, 200);
        assert_eq!(out.chars().count(), 200, "exactly the cap: {}", out.chars().count());
        assert!(out.ends_with('…'), "truncation is visible: {out:?}");
        assert!(!out.contains('\n'));
        // Still starts at the tail, not the head.
        assert!(out.starts_with("line 6 "), "{out:?}");
    }

    #[test]
    fn tail_for_reason_handles_empty_and_blank_input() {
        assert_eq!(tail_for_reason("", 4, 200), "");
        assert_eq!(tail_for_reason("\n\n   \n", 4, 200), "");
    }

    #[test]
    fn classify_probe_exit_is_conservative() {
        // Exits that prove nothing must not be reported as a decision: in
        // ConnectedShared mode a wrong `Some(false)` costs a retry storm
        // against a port that is still occupied.
        assert_eq!(classify_probe_exit(Some(NO_TOKEN_EXIT), ""), None, "no token");
        assert_eq!(classify_probe_exit(Some(28), ""), None, "curl timeout");
        assert_eq!(classify_probe_exit(Some(127), ""), None, "no curl on the remote");
        assert_eq!(classify_probe_exit(Some(126), ""), None, "curl not executable");
        assert_eq!(classify_probe_exit(Some(255), ""), None, "ssh transport");
        assert_eq!(classify_probe_exit(None, ""), None, "killed by a signal");
        assert_eq!(classify_probe_exit(Some(35), ""), None, "an unmapped curl error");

        // …and the ones that do prove something still do.
        assert_eq!(classify_probe_exit(Some(7), ""), Some(false), "refused");
        assert_eq!(classify_probe_exit(Some(22), ""), Some(false), "401 / foreign");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// We can't easily mock `std::process::id()` or `env!("AIUI_GIT_SHA")`,
    /// so we read them through the same channel the function uses and
    /// build the test body to match. Asymmetric cases (different pid /
    /// sha) just plug in known-bogus values.
    fn our_pid() -> u32 {
        std::process::id()
    }
    fn our_sha() -> &'static str {
        env!("AIUI_GIT_SHA")
    }

    #[test]
    fn probe_self_match_returns_true() {
        let body = format!(
            r#"{{"aiui": true, "version": "0.4.x", "pid": {}, "build_sha": "{}"}}"#,
            our_pid(),
            our_sha()
        );
        assert!(probe_response_is_self(&body));
    }

    #[test]
    fn probe_different_pid_returns_false() {
        let bogus_pid = our_pid().wrapping_add(1);
        let body = format!(
            r#"{{"aiui": true, "pid": {}, "build_sha": "{}"}}"#,
            bogus_pid,
            our_sha()
        );
        assert!(!probe_response_is_self(&body));
    }

    #[test]
    fn probe_different_sha_returns_false() {
        let body = format!(
            r#"{{"aiui": true, "pid": {}, "build_sha": "0000000000000000000000000000000000000000"}}"#,
            our_pid()
        );
        assert!(!probe_response_is_self(&body));
    }

    #[test]
    fn probe_legacy_response_without_pid_sha_returns_false() {
        // Old aiui versions (≤ 0.4.32) ship the body without pid /
        // build_sha. The strict matcher must treat those as foreign —
        // we can't prove they're us, so we won't claim them as
        // shared-forward owners. Otherwise a v0.4.32 zombie + v0.4.33
        // primary would still race.
        let body = r#"{"aiui": true, "version": "0.4.32"}"#;
        assert!(!probe_response_is_self(body));
    }

    #[test]
    fn probe_aiui_false_returns_false() {
        let body = format!(
            r#"{{"aiui": false, "pid": {}, "build_sha": "{}"}}"#,
            our_pid(),
            our_sha()
        );
        assert!(!probe_response_is_self(&body));
    }

    #[test]
    fn probe_invalid_json_returns_false() {
        assert!(!probe_response_is_self("not json"));
        assert!(!probe_response_is_self(""));
    }

    /// `remotes.json` is deserialised as a bare `Vec<String>` with no
    /// validation (`setup::load_remotes`), and startup hands every entry
    /// straight to `ensure`. The guard at the top of `ensure` is the only
    /// check on that path — `add_remote`'s boundary validation is not on it.
    /// Nothing asserted that `ensure` consults the validator at all, so the
    /// guard could be reordered below `entries.insert` or dropped as
    /// "already validated upstream" with every test still green (#211).
    ///
    /// Hermetic: a rejected alias returns *before* the insert, so no task is
    /// spawned and no `ssh` ever runs — an empty snapshot is the proof.
    #[tokio::test]
    async fn ensure_rejects_option_like_alias() {
        let mgr = TunnelManager::new(7777);
        for alias in ["-oProxyCommand=touch /tmp/pwned", "user@-evil", "a b"] {
            // The control: these are rejected because the validator rejects
            // them, not because `ensure` refuses everything.
            assert!(
                !crate::setup::is_valid_host_alias(alias),
                "{alias:?} must be invalid for this test to mean anything"
            );
            mgr.ensure(alias.to_string()).await;
        }
        let snap = mgr.snapshot().await;
        assert!(
            snap.is_empty(),
            "no tunnel task may exist for an alias the validator rejects: {snap:?}"
        );
        // …and the same manager does accept a normal alias, so the emptiness
        // above is the guard's doing and not a dead `ensure`. (Asserted on
        // the validator rather than by calling `ensure`, which would spawn a
        // real `ssh` process from a unit test.)
        assert!(crate::setup::is_valid_host_alias("macmini"));
    }
}
