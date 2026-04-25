use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogRequest {
    pub id: String,
    pub spec: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogResult {
    pub id: String,
    pub cancelled: bool,
    pub result: serde_json::Value,
}

/// How long an unresolved dialog may sit in the registry before opportunistic
/// sweep cancels it. Bound for `cargo`-controlled tweaking later.
pub const DIALOG_TTL: Duration = Duration::from_secs(5 * 60);

/// Hard cap on concurrently registered dialogs. When exceeded, the oldest
/// entry is evicted so the map cannot grow without bound even under bursty
/// load.
pub const DIALOG_HARD_CAP: usize = 16;

struct PendingEntry {
    /// Resolves the `/render` waiter once the user submits or cancels.
    result_tx: oneshot::Sender<DialogResult>,
    /// Resolves the per-render ack waiter the first time the frontend
    /// confirms it received `dialog:show`. Wrapped in `Option` so we can
    /// take it out exactly once.
    ack_tx: Option<oneshot::Sender<()>>,
    created_at: Instant,
}

pub struct DialogState {
    pending: Mutex<HashMap<String, PendingEntry>>,
}

/// Live counters for `/health` and diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub struct DialogStats {
    pub orphan_count: usize,
    pub oldest_age_secs: Option<u64>,
}

impl DialogState {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Registers a new dialog and returns `(id, result_rx, ack_rx)`. The
    /// caller is responsible for surfacing the window + emitting the
    /// `dialog:show` event.
    ///
    /// Performs an opportunistic sweep before insert: TTL-expired entries
    /// are cancelled and removed, and if the hard cap would be exceeded
    /// the oldest entry is evicted. No background reaper is needed.
    pub fn register(
        &self,
    ) -> (
        String,
        oneshot::Receiver<DialogResult>,
        oneshot::Receiver<()>,
    ) {
        let id = Uuid::new_v4().to_string();
        let (result_tx, result_rx) = oneshot::channel();
        let (ack_tx, ack_rx) = oneshot::channel();

        let mut map = self.pending.lock().unwrap();

        // Sweep TTL-expired entries.
        let now = Instant::now();
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, e)| now.duration_since(e.created_at) > DIALOG_TTL)
            .map(|(k, _)| k.clone())
            .collect();
        for stale_id in expired {
            if let Some(entry) = map.remove(&stale_id) {
                // Cancel the waiter with the same shape `cancel()` would use.
                let _ = entry.result_tx.send(DialogResult {
                    id: stale_id,
                    cancelled: true,
                    result: serde_json::Value::Null,
                });
            }
        }

        // Enforce hard cap: if at-or-above limit, evict the single oldest.
        if map.len() >= DIALOG_HARD_CAP {
            if let Some(oldest_id) = map
                .iter()
                .min_by_key(|(_, e)| e.created_at)
                .map(|(k, _)| k.clone())
            {
                if let Some(entry) = map.remove(&oldest_id) {
                    let _ = entry.result_tx.send(DialogResult {
                        id: oldest_id,
                        cancelled: true,
                        result: serde_json::Value::Null,
                    });
                }
            }
        }

        map.insert(
            id.clone(),
            PendingEntry {
                result_tx,
                ack_tx: Some(ack_tx),
                created_at: now,
            },
        );

        (id, result_rx, ack_rx)
    }

    /// Marks the dialog with `id` as having been received by the frontend.
    /// Idempotent: the second call is a silent no-op (oneshot already sent).
    pub fn ack(&self, id: &str) {
        let mut map = self.pending.lock().unwrap();
        if let Some(entry) = map.get_mut(id) {
            if let Some(tx) = entry.ack_tx.take() {
                let _ = tx.send(());
            }
        }
    }

    pub fn complete(&self, id: &str, result: serde_json::Value) {
        let entry = self.pending.lock().unwrap().remove(id);
        if let Some(entry) = entry {
            let _ = entry.result_tx.send(DialogResult {
                id: id.to_string(),
                cancelled: false,
                result,
            });
        }
    }

    pub fn cancel(&self, id: &str) {
        let entry = self.pending.lock().unwrap().remove(id);
        if let Some(entry) = entry {
            let _ = entry.result_tx.send(DialogResult {
                id: id.to_string(),
                cancelled: true,
                result: serde_json::Value::Null,
            });
        }
    }

    /// Snapshot for `/health` / diagnostics. Cheap: one mutex acquire.
    pub fn stats(&self) -> DialogStats {
        let map = self.pending.lock().unwrap();
        if map.is_empty() {
            return DialogStats::default();
        }
        let now = Instant::now();
        let oldest = map
            .values()
            .map(|e| now.duration_since(e.created_at))
            .max()
            .map(|d| d.as_secs());
        DialogStats {
            orphan_count: map.len(),
            oldest_age_secs: oldest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_inserts_entry() {
        let s = DialogState::new();
        let (id, _rx, _ack) = s.register();
        assert!(!id.is_empty());
        assert_eq!(s.stats().orphan_count, 1);
    }

    #[test]
    fn complete_resolves_and_removes() {
        let s = DialogState::new();
        let (id, rx, _ack) = s.register();
        s.complete(&id, serde_json::json!({"ok": true}));
        let r = rx.blocking_recv().unwrap();
        assert!(!r.cancelled);
        assert_eq!(s.stats().orphan_count, 0);
    }

    #[test]
    fn cancel_resolves_and_removes() {
        let s = DialogState::new();
        let (id, rx, _ack) = s.register();
        s.cancel(&id);
        let r = rx.blocking_recv().unwrap();
        assert!(r.cancelled);
        assert_eq!(s.stats().orphan_count, 0);
    }

    #[test]
    fn ack_fires_once() {
        let s = DialogState::new();
        let (id, _rx, ack) = s.register();
        s.ack(&id);
        ack.blocking_recv().expect("first ack must arrive");
        // Second ack on the same id is a silent no-op.
        s.ack(&id);
    }

    #[test]
    fn hard_cap_evicts_oldest() {
        let s = DialogState::new();
        let mut rxs = Vec::new();
        for _ in 0..DIALOG_HARD_CAP {
            let (_id, rx, _ack) = s.register();
            rxs.push(rx);
        }
        assert_eq!(s.stats().orphan_count, DIALOG_HARD_CAP);

        // One more — should evict the oldest.
        let (_id, _rx, _ack) = s.register();
        assert_eq!(s.stats().orphan_count, DIALOG_HARD_CAP);

        // The first registered receiver should now resolve as cancelled.
        let first = rxs.remove(0).blocking_recv().unwrap();
        assert!(first.cancelled);
    }
}
