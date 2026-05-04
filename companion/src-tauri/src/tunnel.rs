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
async fn probe_remote_shared_forward(host: &str, port: u16) -> Option<bool> {
    let url = format!("http://localhost:{port}/probe");
    // Read the token *on the remote* and auth-bind the curl in one shell
    // command so the token never appears in our local argv. -f makes curl
    // return non-zero on 4xx/5xx (so 401 = not-shared); -m 3 caps total
    // time.
    let cmd = format!(
        "T=$(cat ~/.config/aiui/token 2>/dev/null) && \
         [ -n \"$T\" ] && \
         curl -sS -f -m 3 -H \"Authorization: Bearer $T\" {url} 2>/dev/null"
    );
    let out = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
            &cmd,
        ])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            let body = String::from_utf8_lossy(&o.stdout);
            Some(probe_response_is_self(&body))
        }
        Ok(o) => {
            // ssh ran, but the remote command failed (curl 401, port
            // empty, missing token, …). exit 255 from ssh itself means
            // "connection error" — keep that as inconclusive so we don't
            // oscillate.
            if o.status.code() == Some(255) {
                None
            } else {
                Some(false)
            }
        }
        Err(_) => None,
    }
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

        let mut child = match Command::new("ssh")
            .args([
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
                &host,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let err = format!("spawn ssh failed: {e}");
                trace(&format!("tunnel[{host}]: {err}"));
                *status.lock().await = TunnelStatus::Failed { reason: err };
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
                    _ = &mut cancel_pin => {
                        *status.lock().await = TunnelStatus::Stopped;
                        return;
                    }
                }
                backoff_secs = (backoff_secs * 2).min(30);
                continue;
            }
        };

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
                let msg = match wait_res {
                    Ok(s) => s
                        .code()
                        .map(|c| format!("ssh exit code {c}"))
                        .unwrap_or_else(|| "ssh killed by signal".to_string()),
                    Err(e) => format!("wait error: {e}"),
                };
                trace(&format!("tunnel[{host}]: ssh died: {msg}"));

                // Before falling into the backoff loop, check if the remote
                // actually has our port forwarded by somebody else. If so,
                // degrade gracefully to shared-forward polling instead of
                // spamming `ssh -NTR` that's guaranteed to keep failing
                // while the zombie session holds the port.
                if let Some(true) = probe_remote_shared_forward(&host, port).await {
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
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
                    _ = &mut cancel_pin => {
                        *status.lock().await = TunnelStatus::Stopped;
                        return;
                    }
                }
                backoff_secs = (backoff_secs * 2).min(30);
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
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(SHARED_FORWARD_POLL_SECS)) => {
                match probe_remote_shared_forward(host, port).await {
                    Some(true) => continue,
                    Some(false) => {
                        trace(&format!(
                            "tunnel[{host}]: shared forward gone — will re-attempt ssh -NTR"
                        ));
                        return PollOutcome::LostShare;
                    }
                    None => {
                        // Inconclusive (ssh itself failed). Keep the label
                        // as shared; next poll will retry. If it's really
                        // gone, the probe will eventually return Some(false).
                        trace(&format!(
                            "tunnel[{host}]: shared-forward probe inconclusive, keeping state"
                        ));
                        let _ = status;
                    }
                }
            }
            _ = &mut *cancel_pin => {
                return PollOutcome::Cancelled;
            }
        }
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
}
