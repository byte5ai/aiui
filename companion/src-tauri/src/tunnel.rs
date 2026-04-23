//! SSH reverse-tunnel manager. Owns one tokio task per remote host, respawns
//! on exit with exponential backoff, and can be cancelled per-host or
//! globally. Status snapshot is exposed to the frontend.

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
    Failed { reason: String },
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
                        .unwrap_or_else(|| format!("ssh killed by signal")),
                    Err(e) => format!("wait error: {e}"),
                };
                trace(&format!("tunnel[{host}]: ssh died: {msg}"));
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
