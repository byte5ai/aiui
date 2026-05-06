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
    /// Why the dialog ended. `None` for normal user-driven submit/cancel
    /// (the existing semantics — `cancelled` alone tells you which).
    /// `Some("ttl_expired")` when the registry sweep cancelled an entry
    /// that sat unresolved past `DIALOG_TTL`. `Some("evicted")` when the
    /// hard-cap kicked in and the oldest entry got pushed out. Lets
    /// callers distinguish "user said no" from "we gave up on this
    /// dialog" — and lets the tracelog explain why a render-call ended
    /// without user input. Issue #H-5 in v0.4.10 review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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

/// Initial inner-size estimate for the dialog window, derived from the
/// rendered spec. Pure function so the caller (`/render`) can size the
/// window before the Svelte side mounts. The user can resize freely
/// afterwards (the window is `resizable(true)` since v0.4.40).
///
/// Heuristics, in order of effect:
///   • `tabs` → +40 px header for the tab bar
///   • Wide widgets (`wireframe` ≥ 3 columns, `mermaid`, `image_grid`
///     ≥ 4 columns, `table` ≥ 4 columns) bump the **width** to fit
///   • Per field, an estimated content height is added; chrome (header
///     + title + footer ≈ 220 px) is included in the base height of
///     480 px and only exceeded once content actually warrants it
///   • Output is clamped to (1100, 900) so we never spawn a window
///     bigger than a small laptop screen
///
/// Confirm/ask specs (no `fields` array) keep the base size — they're
/// short and look right at 520×480.
pub fn estimate_dialog_size(spec: &serde_json::Value) -> (f64, f64) {
    const BASE_W: f64 = 520.0;
    const BASE_H: f64 = 480.0;
    const MAX_W: f64 = 1100.0;
    const MAX_H: f64 = 900.0;
    /// Approx. vertical pixels eaten by header chip + title + description
    /// + footer-with-buttons. Fields below this threshold fit in the
    /// base height with no extra room needed.
    const CHROME_H: f64 = 220.0;

    let mut width = BASE_W;
    let mut content_h: f64 = 0.0;

    if let Some(tabs) = spec.get("tabs").and_then(|v| v.as_array()) {
        if !tabs.is_empty() {
            content_h += 40.0;
        }
    }

    for field in collect_visible_fields(spec) {
        let kind = field.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let (w_min, h_add) = match kind {
            "wireframe" => {
                let cols = field
                    .get("columns")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .max(1);
                let cols_w = match cols {
                    1..=2 => BASE_W,
                    3 => 720.0,
                    _ => 880.0,
                };
                let panels = field
                    .get("panels")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let rows = ((panels as f64) / (cols as f64)).ceil();
                (cols_w, (rows * 100.0).max(150.0))
            }
            "mermaid" => (720.0, 280.0),
            "table" => {
                let cols = field
                    .get("columns")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let w = if cols >= 4 { 720.0 } else { BASE_W };
                let rows = field
                    .get("rows")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                (w, (rows.min(8) as f64 * 32.0 + 80.0).max(180.0))
            }
            "image_grid" => {
                let cols = field
                    .get("columns")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3)
                    .max(1);
                let w = if cols >= 4 { 720.0 } else { BASE_W };
                let images = field
                    .get("images")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let rows = ((images as f64) / (cols as f64)).ceil();
                (w, (rows * 130.0 + 30.0).max(160.0))
            }
            "image" => (BASE_W, 220.0),
            "tree" => (BASE_W, 240.0),
            "list" => {
                let items = field
                    .get("items")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                (BASE_W, (items as f64 * 36.0 + 40.0).clamp(80.0, 320.0))
            }
            "markdown" | "static_text" => (BASE_W, 80.0),
            // Standard inputs (text, password, number, select, checkbox,
            // slider, date, datetime, date_range, color, …) and the
            // catch-all for anything we forgot.
            _ => (BASE_W, 60.0),
        };
        width = width.max(w_min);
        content_h += h_add;
    }

    let needed_h = CHROME_H + content_h;
    let height = if needed_h > BASE_H {
        needed_h.min(MAX_H)
    } else {
        BASE_H
    };
    (width.min(MAX_W), height)
}

fn collect_visible_fields(spec: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut out = Vec::new();
    if let Some(tabs) = spec.get("tabs").and_then(|v| v.as_array()) {
        for tab in tabs {
            if let Some(fields) = tab.get("fields").and_then(|v| v.as_array()) {
                out.extend(fields.iter());
            }
        }
    } else if let Some(fields) = spec.get("fields").and_then(|v| v.as_array()) {
        out.extend(fields.iter());
    }
    out
}

/// Returned by `try_register` when a dialog is already in flight.
/// Surfaced via /render as a 409 so the calling agent can distinguish
/// "companion not reachable" from "companion is busy with someone
/// else's dialog right now". v0.4.36.
#[derive(Debug, Clone, Copy)]
pub struct BusyInfo {
    pub pending_count: usize,
    pub oldest_age_secs: u64,
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
    ///
    /// As of v0.4.36 the production /render path uses `try_register`
    /// instead, which rejects rather than evicts when a dialog is
    /// already in flight. `register` is retained as the
    /// hard-cap-defense fallback for tests and any future call site
    /// that legitimately wants eviction semantics.
    #[allow(dead_code)]
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
                let _ = entry.result_tx.send(DialogResult {
                    id: stale_id,
                    cancelled: true,
                    result: serde_json::Value::Null,
                    reason: Some("ttl_expired".into()),
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
                        reason: Some("evicted".into()),
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
                reason: None,
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
                reason: None,
            });
        }
    }

    /// Like `register` but rejects with `BusyInfo` if a dialog is already
    /// in flight after the TTL sweep. Used by `/render` so that two
    /// parallel callers (multiple aiui calls in one assistant turn,
    /// two Claude sessions hitting the same companion, a stale window
    /// from a previous timeout) can't silently overlay each other —
    /// the second caller gets a clear conflict response instead of
    /// having its predecessor's dialog evicted underfoot. v0.4.36.
    ///
    /// `register` is kept for tests and for any future call site that
    /// genuinely wants the eviction-based behaviour, but the
    /// production /render path uses `try_register` exclusively.
    pub fn try_register(
        &self,
    ) -> Result<
        (
            String,
            oneshot::Receiver<DialogResult>,
            oneshot::Receiver<()>,
        ),
        BusyInfo,
    > {
        let mut map = self.pending.lock().unwrap();

        // Sweep TTL-expired entries first — those don't count as
        // "in flight" any more. Same logic as `register`.
        let now = Instant::now();
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, e)| now.duration_since(e.created_at) > DIALOG_TTL)
            .map(|(k, _)| k.clone())
            .collect();
        for stale_id in expired {
            if let Some(entry) = map.remove(&stale_id) {
                let _ = entry.result_tx.send(DialogResult {
                    id: stale_id,
                    cancelled: true,
                    result: serde_json::Value::Null,
                    reason: Some("ttl_expired".into()),
                });
            }
        }

        if !map.is_empty() {
            let oldest_age_secs = map
                .values()
                .map(|e| now.duration_since(e.created_at).as_secs())
                .max()
                .unwrap_or(0);
            return Err(BusyInfo {
                pending_count: map.len(),
                oldest_age_secs,
            });
        }

        let id = Uuid::new_v4().to_string();
        let (result_tx, result_rx) = oneshot::channel();
        let (ack_tx, ack_rx) = oneshot::channel();
        map.insert(
            id.clone(),
            PendingEntry {
                result_tx,
                ack_tx: Some(ack_tx),
                created_at: now,
            },
        );
        Ok((id, result_rx, ack_rx))
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
    fn estimate_size_confirm_keeps_base() {
        let spec = serde_json::json!({ "kind": "confirm", "title": "ok?" });
        let (w, h) = estimate_dialog_size(&spec);
        assert_eq!(w, 520.0);
        assert_eq!(h, 480.0);
    }

    #[test]
    fn estimate_size_short_form_keeps_base() {
        let spec = serde_json::json!({
            "kind": "form",
            "fields": [
                { "kind": "text", "name": "a" },
                { "kind": "checkbox", "name": "b" }
            ]
        });
        let (w, h) = estimate_dialog_size(&spec);
        assert_eq!(w, 520.0);
        assert_eq!(h, 480.0);
    }

    #[test]
    fn estimate_size_long_form_grows_height_only() {
        let mut fields = Vec::new();
        for i in 0..10 {
            fields.push(serde_json::json!({ "kind": "text", "name": format!("f{i}") }));
        }
        let spec = serde_json::json!({ "kind": "form", "fields": fields });
        let (w, h) = estimate_dialog_size(&spec);
        assert_eq!(w, 520.0);
        assert!(h > 480.0, "10-field form should exceed base height, got {h}");
        assert!(h <= 900.0, "must clamp to MAX_H");
    }

    #[test]
    fn estimate_size_wireframe_3col_widens() {
        let spec = serde_json::json!({
            "kind": "form",
            "fields": [
                { "kind": "wireframe", "columns": 3, "panels": [
                    { "title": "A" }, { "title": "B" }, { "title": "C" }
                ]}
            ]
        });
        let (w, _h) = estimate_dialog_size(&spec);
        assert_eq!(w, 720.0);
    }

    #[test]
    fn estimate_size_mermaid_widens_and_grows() {
        let spec = serde_json::json!({
            "kind": "form",
            "fields": [{ "kind": "mermaid", "source": "graph TD; A-->B" }]
        });
        let (w, h) = estimate_dialog_size(&spec);
        assert_eq!(w, 720.0);
        assert!(h > 480.0, "mermaid should push height past base, got {h}");
    }

    #[test]
    fn estimate_size_clamps_to_max() {
        let mut fields = Vec::new();
        for i in 0..50 {
            fields.push(serde_json::json!({ "kind": "text", "name": format!("f{i}") }));
        }
        let spec = serde_json::json!({ "kind": "form", "fields": fields });
        let (_w, h) = estimate_dialog_size(&spec);
        assert_eq!(h, 900.0, "50-field form must clamp to MAX_H");
    }

    #[test]
    fn estimate_size_walks_into_tabs() {
        let spec = serde_json::json!({
            "kind": "form",
            "tabs": [
                { "label": "T1", "fields": [
                    { "kind": "wireframe", "columns": 4, "panels": [{}, {}, {}, {}] }
                ]}
            ]
        });
        let (w, _h) = estimate_dialog_size(&spec);
        assert_eq!(w, 880.0, "4-col wireframe inside tab should still widen");
    }

    #[test]
    fn try_register_succeeds_when_empty() {
        let s = DialogState::new();
        let res = s.try_register();
        assert!(res.is_ok());
        assert_eq!(s.stats().orphan_count, 1);
    }

    #[test]
    fn try_register_rejects_when_pending() {
        let s = DialogState::new();
        let (_id, _rx, _ack) = s.try_register().expect("first try_register");
        let busy = s.try_register().expect_err("second try_register must be busy");
        assert_eq!(busy.pending_count, 1);
        // Registry still holds the original entry.
        assert_eq!(s.stats().orphan_count, 1);
    }

    #[test]
    fn try_register_succeeds_after_complete() {
        let s = DialogState::new();
        let (id, _rx, _ack) = s.try_register().expect("first");
        s.complete(&id, serde_json::json!({"ok": true}));
        let res = s.try_register();
        assert!(res.is_ok());
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
