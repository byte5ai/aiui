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
    /// Our API token — used only to VERIFY a shared-forward probe's answer,
    /// never sent anywhere (review B1-01).
    token: Arc<str>,
}

impl TunnelManager {
    pub fn new(port: u16, token: &str) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            port,
            token: Arc::from(token),
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
        let token = self.token.clone();
        tokio::spawn(async move {
            run_tunnel(host, port, token, cancel_rx, status).await;
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
/// Review B1-01: the probe no longer touches the token at all. It used to
/// read `~/.config/aiui/token` and send it as a bearer header to whatever
/// listened on the remote port — and the probe runs exactly when our own
/// forward is DOWN (after every `ssh -NTR` death, then every 30 s in shared
/// mode), so a co-tenant squatting the port collected the token on every
/// cycle. Now the companion sends a fresh random nonce and verifies the
/// answer's HMAC itself ([`probe_response_is_self`]); nothing secret leaves
/// the Mac. (#187's argv concern is moot for the same reason.)
///
/// `nonce` is lowercase hex generated here, so it is safe in the URL. `-f`
/// makes curl fail on 4xx/5xx (a foreign service, or an older aiui that does
/// not know the challenge, reads as not-shared) and `-m 3` caps its time.
///
/// A function rather than an inline `format!` so the tests exercise the
/// string that actually ships.
fn probe_command(port: u16, nonce: &str) -> String {
    format!("curl -sS -f -m 3 'http://localhost:{port}/probe?nonce={nonce}'")
}

/// A fresh challenge nonce: 32 random bytes as 64 hex characters.
fn new_probe_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

async fn probe_remote_shared_forward(host: &str, port: u16, token: &str) -> Option<bool> {
    let nonce = new_probe_nonce();
    let cmd = probe_command(port, &nonce);
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
            let verdict = classify_probe_exit(o.status.code(), &stdout, &nonce, token);
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
fn classify_probe_exit(code: Option<i32>, stdout: &str, nonce: &str, token: &str) -> Option<bool> {
    match code {
        // curl got a 2xx — the body (and its MAC) decides whether it is us.
        Some(0) => Some(probe_response_is_self(stdout, nonce, token)),
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

/// Truncate `buf` to at most `max` bytes, keeping the TAIL and cutting on a
/// char boundary. Byte-index slicing panicked when the cut fell inside a
/// multi-byte character — a long UTF-8 ssh banner was enough (B1-10).
fn keep_tail(buf: &mut String, max: usize) {
    if buf.len() <= max {
        return;
    }
    let mut cut = buf.len() - max;
    while !buf.is_char_boundary(cut) {
        cut += 1;
    }
    buf.drain(..cut);
}

/// Pause after an authentication failure (B1-15).
const AUTH_FAILURE_BACKOFF: Duration = Duration::from_secs(300);

/// Did ssh fail on authentication rather than on the link? Those failures
/// need the user (a key, `known_hosts`), not a retry.
fn is_auth_failure(stderr_tail: &str) -> bool {
    stderr_tail.contains("Permission denied") || stderr_tail.contains("Host key verification failed")
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
fn probe_response_is_self(body: &str, nonce: &str, token: &str) -> bool {
    probe_response_is_from(body, nonce, token, std::process::id(), env!("AIUI_GIT_SHA"))
}

/// Pure core of [`probe_response_is_self`]: the body must name `our_pid` and
/// `our_sha` AND carry the MAC only a holder of `token` can compute for this
/// `nonce` (review B1-01). Without the MAC, any listener could echo our pid
/// and build sha back — both are visible to every local user on the Mac.
fn probe_response_is_from(body: &str, nonce: &str, token: &str, our_pid: u32, our_sha: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    if !parsed.get("aiui").and_then(|v| v.as_bool()).unwrap_or(false) {
        return false;
    }
    let body_pid = parsed.get("pid").and_then(|v| v.as_u64());
    let body_sha = parsed.get("build_sha").and_then(|v| v.as_str());
    let body_mac = parsed.get("mac").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(body_pid, Some(p) if p == our_pid as u64)
        || !matches!(body_sha, Some(s) if s == our_sha)
        || token.is_empty()
    {
        return false;
    }
    let want = crate::http::probe_mac(token, nonce, our_pid, our_sha);
    crate::http::constant_time_eq(body_mac.as_bytes(), want.as_bytes())
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
    token: Arc<str>,
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
        let mut drain: Option<tokio::task::JoinHandle<()>> = None;
        if let Some(mut err) = child.stderr.take() {
            let sink = stderr_tail.clone();
            drain = Some(tokio::spawn(async move {
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
                            keep_tail(&mut guard, 4096);
                        }
                    }
                }
            }));
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
                // B1-10: `wait()` can resolve before the drain has read
                // ssh's last stderr bytes — the very line ("remote port
                // forwarding failed …") the reason exists to show. Give the
                // drain a moment to reach EOF.
                if let Some(h) = drain.take() {
                    let _ = tokio::time::timeout(Duration::from_millis(500), h).await;
                }
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

                // B1-15: a rejected key or a changed host key will not fix
                // itself in 30 s. Retrying at the normal cadence — plus the
                // shared-forward probe, a second login — meant ~4 failed
                // logins a minute forever, enough for fail2ban to ban the
                // Mac. No probe, and a long pause; Resync restarts at once.
                if is_auth_failure(&tail) {
                    *status.lock().await = TunnelStatus::Failed { reason: msg };
                    tokio::select! {
                        _ = tokio::time::sleep(AUTH_FAILURE_BACKOFF) => {}
                        _ = &mut cancel_pin => {
                            *status.lock().await = TunnelStatus::Stopped;
                            return;
                        }
                    }
                    backoff_secs = 1;
                    continue;
                }

                // Before falling into the backoff loop, check if the remote
                // actually has our port forwarded by somebody else. If so,
                // degrade gracefully to shared-forward polling instead of
                // spamming `ssh -NTR` that's guaranteed to keep failing
                // while the zombie session holds the port.
                // #187: cancellable. A `remove_remote` during this probe
                // used to be ignored until it finished.
                let shared = tokio::select! {
                    v = probe_remote_shared_forward(&host, port, &token) => v,
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
                    match shared_forward_poll_loop(&host, port, &token, &status, &mut cancel_pin).await {
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
    token: &str,
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
            v = probe_remote_shared_forward(host, port, token) => v,
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

    const NONCE: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn the_probe_never_reads_or_sends_the_token() {
        // B1-01: the probe runs exactly while our forward is down, so the
        // listener it reaches may be anyone's. Nothing secret may go there —
        // not as an argument (#187), not as a header, not at all.
        let cmd = probe_cmd(7777, NONCE);
        assert!(!cmd.contains("token"), "{cmd}");
        assert!(!cmd.contains("Authorization"), "{cmd}");
        assert!(!cmd.contains("Bearer"), "{cmd}");
        assert!(cmd.contains(&format!("/probe?nonce={NONCE}")), "{cmd}");
    }

    #[test]
    fn a_fresh_nonce_is_a_valid_challenge() {
        let a = new_probe_nonce();
        let b = new_probe_nonce();
        assert!(crate::http::is_probe_nonce(&a), "{a}");
        assert_ne!(a, b, "nonces must not repeat");
    }

    fn answer(nonce: &str, token: &str, pid: u32, sha: &str) -> String {
        serde_json::json!({
            "aiui": true,
            "pid": pid,
            "build_sha": sha,
            "mac": crate::http::probe_mac(token, nonce, pid, sha),
        })
        .to_string()
    }

    #[test]
    fn only_a_holder_of_the_token_passes_the_challenge() {
        let ok = answer(NONCE, "tok", 42, "abc");
        assert!(probe_response_is_from(&ok, NONCE, "tok", 42, "abc"));
        // Echoing our pid and build sha is not enough: both are visible to
        // every local user. This is what the old body-only check accepted.
        let echo = serde_json::json!({"aiui": true, "pid": 42, "build_sha": "abc"}).to_string();
        assert!(!probe_response_is_from(&echo, NONCE, "tok", 42, "abc"));
        // A MAC under another token, or for another nonce (a replay).
        assert!(!probe_response_is_from(&answer(NONCE, "other", 42, "abc"), NONCE, "tok", 42, "abc"));
        let other_nonce = "ff".repeat(32);
        assert!(!probe_response_is_from(&answer(&other_nonce, "tok", 42, "abc"), NONCE, "tok", 42, "abc"));
        // A genuine aiui, but a different process (a second instance).
        assert!(!probe_response_is_from(&answer(NONCE, "tok", 43, "abc"), NONCE, "tok", 42, "abc"));
        // No token configured: never "us".
        assert!(!probe_response_is_from(&answer(NONCE, "", 42, "abc"), NONCE, "", 42, "abc"));
    }

    // The probe itself only ever executes on the remote, which is a POSIX
    // host by construction (it is reached over ssh).
    #[cfg(unix)]
    #[test]
    fn the_command_is_valid_shell() {
        // Syntax-check with a real shell rather than by eye.
        let out = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(probe_cmd(7777, NONCE))
            .output()
            .expect("sh is available");
        assert!(
            out.status.success(),
            "sh -n rejected the probe command: {}",
            String::from_utf8_lossy(&out.stderr)
        );
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
        let raw = "Warning: Permanently added 'example-host' to the list of known hosts.\n\
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
    fn keep_tail_cuts_on_a_char_boundary() {
        // B1-10: a 4 KB cut inside "ä" (2 bytes) or "—" (3 bytes) panicked.
        let mut s = "ä".repeat(3000); // 6000 bytes
        keep_tail(&mut s, 4095);
        assert!(s.len() <= 4095);
        assert!(s.chars().all(|c| c == 'ä'));
        let mut t = "—".repeat(2000);
        keep_tail(&mut t, 4096);
        assert!(t.len() <= 4096 && t.chars().all(|c| c == '—'));
        let mut short = String::from("ok");
        keep_tail(&mut short, 4096);
        assert_eq!(short, "ok");
    }

    #[test]
    fn auth_failures_are_recognised() {
        assert!(is_auth_failure("user@example-host: Permission denied (publickey)."));
        assert!(is_auth_failure("Host key verification failed."));
        assert!(!is_auth_failure("Warning: remote port forwarding failed for listen port 7777"));
        assert!(!is_auth_failure(""));
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
        let c = |code| classify_probe_exit(code, "", NONCE, "tok");
        assert_eq!(c(Some(28)), None, "curl timeout");
        assert_eq!(c(Some(127)), None, "no curl on the remote");
        assert_eq!(c(Some(126)), None, "curl not executable");
        assert_eq!(c(Some(255)), None, "ssh transport");
        assert_eq!(c(None), None, "killed by a signal");
        assert_eq!(c(Some(35)), None, "an unmapped curl error");

        // …and the ones that do prove something still do.
        assert_eq!(c(Some(7)), Some(false), "refused");
        assert_eq!(c(Some(22)), Some(false), "400 / 401 / foreign");
        // A 2xx from a listener that cannot answer the challenge.
        assert_eq!(c(Some(0)), Some(false), "2xx without our MAC");
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

    const N: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn probe_self_match_returns_true() {
        let body = format!(
            r#"{{"aiui": true, "version": "0.4.x", "pid": {}, "build_sha": "{}", "mac": "{}"}}"#,
            our_pid(),
            our_sha(),
            crate::http::probe_mac("tok", N, our_pid(), our_sha())
        );
        assert!(probe_response_is_self(&body, N, "tok"));
    }

    #[test]
    fn probe_different_pid_returns_false() {
        let bogus_pid = our_pid().wrapping_add(1);
        let body = format!(
            r#"{{"aiui": true, "pid": {}, "build_sha": "{}"}}"#,
            bogus_pid,
            our_sha()
        );
        assert!(!probe_response_is_self(&body, N, "tok"));
    }

    #[test]
    fn probe_different_sha_returns_false() {
        let body = format!(
            r#"{{"aiui": true, "pid": {}, "build_sha": "0000000000000000000000000000000000000000"}}"#,
            our_pid()
        );
        assert!(!probe_response_is_self(&body, N, "tok"));
    }

    #[test]
    fn probe_legacy_response_without_pid_sha_returns_false() {
        // Old aiui versions (≤ 0.4.32) ship the body without pid /
        // build_sha. The strict matcher must treat those as foreign —
        // we can't prove they're us, so we won't claim them as
        // shared-forward owners. Otherwise a v0.4.32 zombie + v0.4.33
        // primary would still race.
        let body = r#"{"aiui": true, "version": "0.4.32"}"#;
        assert!(!probe_response_is_self(body, N, "tok"));
    }

    #[test]
    fn probe_aiui_false_returns_false() {
        let body = format!(
            r#"{{"aiui": false, "pid": {}, "build_sha": "{}"}}"#,
            our_pid(),
            our_sha()
        );
        assert!(!probe_response_is_self(&body, N, "tok"));
    }

    #[test]
    fn probe_invalid_json_returns_false() {
        assert!(!probe_response_is_self("not json", N, "tok"));
        assert!(!probe_response_is_self("", N, "tok"));
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
        let mgr = TunnelManager::new(7777, "test-token");
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
