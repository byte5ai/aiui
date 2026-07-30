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
    /// Companion-side TTL for this dialog, in seconds. Frontend reads
    /// this to schedule warning banners (T-15 min, T-2 min) and an
    /// auto-cancel slightly before the backend sweeps. Single source
    /// of truth for "how long the user has" is here in Rust. v0.4.41.
    pub ttl_secs: u64,
    /// Human-legible session label the caller passed (project name, task,
    /// etc.) so the user can tell which session a dialog belongs to when
    /// several are open at once (Invariant I8). `None` if the caller passed
    /// nothing — the window then falls back to `session_origin` + short id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Origin host of the caller, auto-injected by the remote Python bridge
    /// (its `hostname`) since the Mac can't distinguish remotes sharing
    /// `:7777`. `None`/absent for local callers. Shown in the window chrome
    /// alongside `session` (I8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<String>,
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
/// sweep cancels it. Two hours — long enough to cover any realistic form
/// the user could fill out without artificial pressure, short enough to
/// auto-recover from a genuinely stuck WebView. v0.4.41 raised from 5 min
/// after the 2026-05-06 Weekly-Planner feedback where complex forms hit
/// the prior limit mid-fill. Frontend gets the value (via
/// `DialogRequest.ttl_secs`) and surfaces it as countdown warnings.
pub const DIALOG_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// Hard cap on concurrently registered dialogs. When exceeded, the oldest
/// entry is evicted so the map cannot grow without bound even under bursty
/// load.
pub const DIALOG_HARD_CAP: usize = 16;

struct PendingEntry {
    /// Resolves the `/render` waiter once the user submits or cancels.
    result_tx: oneshot::Sender<DialogResult>,
    /// The full request payload (spec, ttl, session chrome). Stored so the
    /// per-id dialog window can *pull* it on mount via `get_dialog_spec`
    /// (Step 4 multi-window) instead of the backend emitting + waiting for an
    /// ack — the pull model removes the event-ordering race the old
    /// `dialog:show` + `dialog_window_ready` handshake existed to paper over.
    request: DialogRequest,
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
    // Approx. vertical pixels eaten by header chip + title + description
    // + footer-with-buttons. Fields below this threshold fit in the
    // base height with no extra room needed.
    const CHROME_H: f64 = 220.0;

    let mut width = BASE_W;
    let mut content_h: f64 = 0.0;

    // Gallery has no form `fields`; it is a grid of item cards. Size by
    // item count and column layout so a batch review opens roomy.
    if spec.get("kind").and_then(|v| v.as_str()) == Some("gallery") {
        let items = spec
            .get("items")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let cols = spec
            .get("columns")
            .and_then(|v| v.as_u64())
            .filter(|&c| c > 0)
            .unwrap_or(if items >= 4 { 3 } else { 2 })
            .max(1);
        let w = match cols {
            1 => BASE_W,
            2 => 680.0,
            3 => 880.0,
            _ => MAX_W,
        };
        let rows = ((items as f64) / (cols as f64)).ceil();
        // Each card ≈ thumbnail (200) + label/detail + action row + optional comment.
        let per_card = if spec.get("comment").and_then(|v| v.as_bool()).unwrap_or(false) {
            330.0
        } else {
            290.0
        };
        let needed = CHROME_H + (rows * per_card).max(per_card);
        return (w.min(MAX_W), needed.clamp(BASE_H, MAX_H));
    }

    // Compare has no `fields` either; it's N equal-width panes shown side
    // by side. Width scales with the pane count (capped at 4, matching the
    // frontend's grid cap); height is generous by default since panes
    // typically hold either a full paragraph of markdown or an image.
    if spec.get("kind").and_then(|v| v.as_str()) == Some("compare") {
        let variants = spec
            .get("variants")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(2)
            .max(1);
        let cols = spec
            .get("columns")
            .and_then(|v| v.as_u64())
            .filter(|&c| c > 0)
            .map(|c| c as usize)
            .unwrap_or(variants)
            .clamp(1, 4);
        let w = match cols {
            1 => BASE_W,
            2 => 760.0,
            3 => 980.0,
            _ => MAX_W,
        };
        let has_media = spec
            .get("variants")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|it| {
                    it.get("src")
                        .and_then(|s| s.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        let h: f64 = if has_media { 640.0 } else { 560.0 };
        return (w.min(MAX_W), h.clamp(BASE_H, MAX_H));
    }

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
            "annotated_image" => {
                // Interactive: image + tool/clear toolbar + coordinate readout.
                // Reserve extra vertical room over a plain `image` so the stage
                // isn't cramped and the drag surface stays usable.
                let img_h = field
                    .get("max_height")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(320.0);
                (BASE_W, (img_h + 90.0).min(560.0))
            }
            // Audio (#25): just a native `<audio controls>` bar (~40px) plus
            // an optional caption — far shallower than an image preview.
            "audio" => (BASE_W, 90.0),
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

/// Agent-facing start-size presets, in logical px. The agent may pass
/// `size: "s" | "m" | "l"` on any dialog spec to ask for a roomier starting
/// window. Picked to be comfortable defaults on a typical laptop screen; the
/// real upper bound is the monitor-work-area clamp applied at build time.
fn size_preset(name: &str) -> Option<(f64, f64)> {
    match name.trim().to_ascii_lowercase().as_str() {
        "s" | "small" => Some((520.0, 480.0)),
        "m" | "medium" => Some((760.0, 620.0)),
        "l" | "large" => Some((1040.0, 820.0)),
        _ => None,
    }
}

/// Hard ceiling for an *explicit* `width`/`height` hint, independent of the
/// auto-estimate's own cap. The monitor-work-area clamp in
/// `build_dialog_window` is the real upper bound; this just stops a wild
/// number from constructing an absurd window before that clamp runs.
const HINT_MAX_W: f64 = 1600.0;
const HINT_MAX_H: f64 = 1200.0;

/// Resolve the *starting* inner size for a dialog window, combining the
/// content estimate with an optional agent-supplied size hint.
///
/// The hint comes from either explicit `width`/`height` (logical px, take
/// precedence) or a `size: "s"|"m"|"l"` preset. It acts as a **floor**, not
/// an override: the window opens at `max(content-estimate, hint)` per
/// dimension. That means a content-heavy dialog never opens smaller than its
/// content needs (so `size:"s"` can't cram a 12-image gallery), while a light
/// dialog *can* be asked to start large (so a sparse form with `size:"l"`
/// opens roomy instead of at the cramped base size). Addresses the
/// 2026-05-31 report: dialogs opened too small and users didn't know they
/// could drag-resize. An unrecognised `size` value falls back to pure
/// auto-sizing — no error, since the window is resizable regardless.
pub fn resolve_start_size(spec: &serde_json::Value) -> (f64, f64) {
    let (auto_w, auto_h) = estimate_dialog_size(spec);

    let explicit_w = spec
        .get("width")
        .and_then(|v| v.as_f64())
        .filter(|w| *w > 0.0);
    let explicit_h = spec
        .get("height")
        .and_then(|v| v.as_f64())
        .filter(|h| *h > 0.0);
    let preset = spec
        .get("size")
        .and_then(|v| v.as_str())
        .and_then(size_preset);

    let hint_w = explicit_w.or(preset.map(|p| p.0)).unwrap_or(0.0);
    let hint_h = explicit_h.or(preset.map(|p| p.1)).unwrap_or(0.0);

    (
        auto_w.max(hint_w).min(HINT_MAX_W),
        auto_h.max(hint_h).min(HINT_MAX_H),
    )
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

impl DialogState {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new dialog and return `(id, result_rx)`. Multi-window
    /// (Step 4, Invariant I8): N dialogs may be in flight at once — this never
    /// rejects (the old single-occupancy 409 is gone). It sweeps TTL-expired
    /// entries and, only if the hard cap would be exceeded, evicts the single
    /// oldest. The caller (`/render`) builds a per-id window; the window pulls
    /// the stored `DialogRequest` via [`Self::get_request`] on mount.
    pub fn register_dialog(
        &self,
        spec: serde_json::Value,
        session: Option<String>,
        session_origin: Option<String>,
        ttl_secs: u64,
    ) -> (String, oneshot::Receiver<DialogResult>) {
        let id = Uuid::new_v4().to_string();
        let (result_tx, result_rx) = oneshot::channel();

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

        let request = DialogRequest {
            id: id.clone(),
            spec,
            ttl_secs,
            session,
            session_origin,
        };
        map.insert(
            id.clone(),
            PendingEntry {
                result_tx,
                request,
                created_at: now,
            },
        );

        (id, result_rx)
    }

    /// Return a clone of the stored request for `id`, for the per-id dialog
    /// window to pull on mount (Step 4 pull model). `None` if the dialog is
    /// gone (already resolved, evicted, or never existed) — the window then
    /// closes itself.
    pub fn get_request(&self, id: &str) -> Option<DialogRequest> {
        self.pending.lock().unwrap().get(id).map(|e| e.request.clone())
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

    // (Step 4 removed `cancel_all`: multi-window cancels per-id — the
    // X-close handler cancels the closed window's own dialog by its label-id,
    // and `sweep_orphan_dialog_window` reaps each orphan window individually.
    // A blunt "cancel everything" would wrongly tear down other sessions'
    // live dialogs.)

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

    fn reg(s: &DialogState) -> (String, oneshot::Receiver<DialogResult>) {
        s.register_dialog(serde_json::json!({"kind": "confirm", "title": "?"}), None, None, 0)
    }

    #[test]
    fn register_inserts_entry() {
        let s = DialogState::new();
        let (id, _rx) = reg(&s);
        assert!(!id.is_empty());
        assert_eq!(s.stats().orphan_count, 1);
    }

    #[test]
    fn complete_resolves_and_removes() {
        let s = DialogState::new();
        let (id, rx) = reg(&s);
        s.complete(&id, serde_json::json!({"ok": true}));
        let r = rx.blocking_recv().unwrap();
        assert!(!r.cancelled);
        assert_eq!(s.stats().orphan_count, 0);
    }

    #[test]
    fn cancel_resolves_and_removes() {
        let s = DialogState::new();
        let (id, rx) = reg(&s);
        s.cancel(&id);
        let r = rx.blocking_recv().unwrap();
        assert!(r.cancelled);
        assert_eq!(s.stats().orphan_count, 0);
    }

    #[test]
    fn multiple_dialogs_register_concurrently_no_409() {
        // Step 4 / I8: single-occupancy is gone — N dialogs coexist.
        let s = DialogState::new();
        let (_a, _ra) = reg(&s);
        let (_b, _rb) = reg(&s);
        let (_c, _rc) = reg(&s);
        assert_eq!(s.stats().orphan_count, 3);
    }

    #[test]
    fn get_request_returns_stored_payload_then_none_after_resolve() {
        let s = DialogState::new();
        let (id, _rx) = s.register_dialog(
            serde_json::json!({"kind": "confirm"}),
            Some("my-project".into()),
            Some("macmini".into()),
            42,
        );
        let req = s.get_request(&id).expect("request stored for pull");
        assert_eq!(req.id, id);
        assert_eq!(req.ttl_secs, 42);
        assert_eq!(req.session.as_deref(), Some("my-project"));
        assert_eq!(req.session_origin.as_deref(), Some("macmini"));
        // Once resolved, the pull returns None so the window closes itself.
        s.complete(&id, serde_json::json!({}));
        assert!(s.get_request(&id).is_none());
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
    fn estimate_size_audio_stays_narrow_and_short() {
        let spec = serde_json::json!({
            "kind": "form",
            "fields": [{ "kind": "audio", "src": "data:audio/mpeg;base64,AAAA", "label": "Sample" }]
        });
        let (w, h) = estimate_dialog_size(&spec);
        assert_eq!(w, 520.0, "audio field should not widen the window");
        assert_eq!(h, 480.0, "a single short audio field should stay at base height");
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
    fn estimate_size_gallery_scales_with_items() {
        // Few items → narrower 2-col layout, roomy but bounded height.
        let small = serde_json::json!({
            "kind": "gallery",
            "items": [
                { "value": "a", "src": "data:image/png;base64,AAAA" },
                { "value": "b", "src": "data:image/png;base64,BBBB" }
            ]
        });
        let (w_small, h_small) = estimate_dialog_size(&small);
        assert_eq!(w_small, 680.0, "2 items → 2-col → 680 wide");
        assert!((480.0..=900.0).contains(&h_small));

        // Many items → 3-col grid widens, more rows push height higher.
        let mut items = Vec::new();
        for i in 0..9 {
            items.push(serde_json::json!({ "value": format!("v{i}"), "src": "data:image/png;base64,AAAA" }));
        }
        let big = serde_json::json!({ "kind": "gallery", "items": items });
        let (w_big, h_big) = estimate_dialog_size(&big);
        assert_eq!(w_big, 880.0, "≥4 items → 3-col → 880 wide");
        assert!(h_big > h_small, "9 items should be taller than 2, got {h_big} vs {h_small}");
        assert!(h_big <= 900.0, "must clamp to MAX_H");
    }

    #[test]
    fn estimate_size_gallery_respects_explicit_columns() {
        let spec = serde_json::json!({
            "kind": "gallery",
            "columns": 1,
            "items": [{ "value": "a" }, { "value": "b" }]
        });
        let (w, _h) = estimate_dialog_size(&spec);
        assert_eq!(w, 520.0, "explicit 1 column → base width");
    }

    #[test]
    fn estimate_size_compare_scales_with_variant_count() {
        let two = serde_json::json!({
            "kind": "compare",
            "variants": [
                { "value": "a", "content": "Draft A" },
                { "value": "b", "content": "Draft B" }
            ]
        });
        let (w2, h2) = estimate_dialog_size(&two);
        assert_eq!(w2, 760.0, "2 variants → 760 wide");
        assert_eq!(h2, 560.0, "no media → 560 tall");

        let three = serde_json::json!({
            "kind": "compare",
            "variants": [
                { "value": "a", "content": "A" },
                { "value": "b", "content": "B" },
                { "value": "c", "content": "C" }
            ]
        });
        let (w3, _h3) = estimate_dialog_size(&three);
        assert_eq!(w3, 980.0, "3 variants → 980 wide");
    }

    #[test]
    fn estimate_size_compare_grows_taller_with_media() {
        let spec = serde_json::json!({
            "kind": "compare",
            "variants": [
                { "value": "a", "src": "data:image/png;base64,AAAA" },
                { "value": "b", "src": "data:image/png;base64,BBBB" }
            ]
        });
        let (_w, h) = estimate_dialog_size(&spec);
        assert_eq!(h, 640.0, "image variants → taller default than text-only");
    }

    #[test]
    fn estimate_size_compare_respects_explicit_columns_cap() {
        let mut variants = Vec::new();
        for i in 0..6 {
            variants.push(serde_json::json!({ "value": format!("v{i}"), "content": "x" }));
        }
        let spec = serde_json::json!({ "kind": "compare", "variants": variants });
        let (w, _h) = estimate_dialog_size(&spec);
        assert_eq!(w, 1100.0, "variant count above the 4-col cap → MAX_W");
    }

    #[test]
    fn resolve_start_size_no_hint_equals_estimate() {
        let spec = serde_json::json!({ "kind": "confirm", "title": "ok?" });
        assert_eq!(resolve_start_size(&spec), estimate_dialog_size(&spec));
    }

    #[test]
    fn resolve_start_size_preset_floors_a_small_dialog() {
        // A bare confirm auto-sizes to the base (520×480). Asking for "l"
        // opens it large instead.
        let spec = serde_json::json!({ "kind": "confirm", "title": "ok?", "size": "l" });
        let (w, h) = resolve_start_size(&spec);
        assert_eq!((w, h), (1040.0, 820.0));

        let spec_m = serde_json::json!({ "kind": "confirm", "title": "ok?", "size": "m" });
        assert_eq!(resolve_start_size(&spec_m), (760.0, 620.0));
    }

    #[test]
    fn resolve_start_size_preset_is_a_floor_not_a_cap() {
        // 9-item gallery auto-sizes large (880 wide). "s" must NOT shrink it
        // below what the content needs.
        let mut items = Vec::new();
        for i in 0..9 {
            items.push(serde_json::json!({ "value": format!("v{i}"), "src": "data:image/png;base64,AAAA" }));
        }
        let spec = serde_json::json!({ "kind": "gallery", "items": items, "size": "s" });
        let (auto_w, _) = estimate_dialog_size(&serde_json::json!({
            "kind": "gallery",
            "items": (0..9).map(|i| serde_json::json!({"value": format!("v{i}")})).collect::<Vec<_>>()
        }));
        let (w, _) = resolve_start_size(&spec);
        assert!(w >= auto_w, "content estimate must win over a smaller preset: {w} < {auto_w}");
    }

    #[test]
    fn resolve_start_size_explicit_dims_override_preset() {
        let spec = serde_json::json!({
            "kind": "form", "title": "x", "size": "s", "width": 900, "height": 700
        });
        let (w, h) = resolve_start_size(&spec);
        assert_eq!((w, h), (900.0, 700.0));
    }

    #[test]
    fn resolve_start_size_clamps_absurd_explicit_dims() {
        let spec = serde_json::json!({
            "kind": "form", "title": "x", "width": 99999, "height": 99999
        });
        let (w, h) = resolve_start_size(&spec);
        assert_eq!((w, h), (1600.0, 1200.0), "explicit dims clamp to HINT_MAX");
    }

    #[test]
    fn resolve_start_size_ignores_unknown_preset() {
        let spec = serde_json::json!({ "kind": "confirm", "title": "ok?", "size": "humongous" });
        assert_eq!(resolve_start_size(&spec), estimate_dialog_size(&spec));
    }

    #[test]
    fn hard_cap_evicts_oldest() {
        let s = DialogState::new();
        let mut rxs = Vec::new();
        for _ in 0..DIALOG_HARD_CAP {
            let (_id, rx) = reg(&s);
            rxs.push(rx);
        }
        assert_eq!(s.stats().orphan_count, DIALOG_HARD_CAP);

        // One more — should evict the oldest (the cap bounds the map; the
        // 409 single-occupancy that used to reject earlier is gone).
        let (_id, _rx) = reg(&s);
        assert_eq!(s.stats().orphan_count, DIALOG_HARD_CAP);

        // The first registered receiver should now resolve as cancelled.
        let first = rxs.remove(0).blocking_recv().unwrap();
        assert!(first.cancelled);
        assert_eq!(first.reason.as_deref(), Some("evicted"));
    }
}
