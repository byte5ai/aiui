use crate::ack::AckRegistry;
use crate::config::AppConfig;
use crate::dialog::{DialogState, DIALOG_TTL};
use crate::lifetime::LifetimeStats;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use crate::logging::trace;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

/// How long `/health` waits for a `ui:ping` round-trip from the frontend
/// before concluding the WebView is unresponsive.
const UI_PING_TIMEOUT: Duration = Duration::from_millis(100);

/// Header a bridge sets to opt into async `/render` (Step 3). Present →
/// `POST /render` registers + surfaces the dialog, returns `{id, ttl}`
/// immediately (202), and the caller polls `GET /render/{id}`. Absent → the
/// legacy synchronous long-poll (POST holds the connection until the user
/// answers). Backward-compatible: old bridges that don't set it keep working
/// unchanged, so the wire contract stays v1.
const ASYNC_RENDER_HEADER: &str = "x-aiui-async";

/// How long a single `GET /render/{id}` long-poll parks before returning
/// `{pending:true}` so the caller can re-poll (and emit a progress
/// notification). Short enough to stay well under any client read timeout, so
/// a tunnel/GUI blip can only ever cost one poll window, never a multi-minute
/// held connection (the remote ReadError class this closes).
const ASYNC_POLL_WINDOW: Duration = Duration::from_secs(25);

/// Buffered terminal result for an async render, keyed by dialog id. The
/// `POST /render` async branch spawns a task that awaits the user's answer and
/// fills this; `GET /render/{id}` drains it. Decouples the dialog's lifetime
/// from any single HTTP connection.
struct AsyncSlot {
    /// `Some` once the dialog reached a terminal outcome; drained by the first
    /// successful GET. A `GET /render/{id}` poll-loops (cheap 200 ms ticks,
    /// bounded by `ASYNC_POLL_WINDOW`) reading this — no cross-task notifier to
    /// reason about, and a missed tick costs at most 200 ms, never correctness.
    result: Option<crate::dialog::DialogResult>,
    /// For the opportunistic sweep of resolved-but-never-collected slots.
    created_at: Instant,
}

#[derive(Clone)]
struct AppState {
    cfg: Arc<AppConfig>,
    dialog: Arc<DialogState>,
    ui_acks: Arc<AckRegistry>,
    lifetime: Arc<LifetimeStats>,
    app: AppHandle,
    /// Buffered terminal results for async renders (Step 3), keyed by dialog
    /// id. Empty in the all-synchronous case.
    async_slots: Arc<Mutex<std::collections::HashMap<String, AsyncSlot>>>,
}

#[derive(Deserialize)]
struct RenderRequest {
    #[serde(default)]
    _timeout_s: Option<u64>,
    spec: serde_json::Value,
    /// Human-legible session label set by the caller (Step 4, I8). Shown in
    /// the dialog window's chrome so the user can tell which session a dialog
    /// belongs to. Optional.
    #[serde(default)]
    session: Option<String>,
    /// Origin host, auto-injected by the remote Python bridge (its hostname).
    /// Optional; absent for local callers.
    #[serde(default)]
    session_origin: Option<String>,
}

#[derive(Serialize)]
struct RenderResponse {
    id: String,
    cancelled: bool,
    result: serde_json::Value,
    /// Cancellation reason if the dialog ended without a user submit —
    /// `ttl_expired`, `evicted`, `channel_dropped`. Omitted on normal
    /// user-driven submit/cancel. Lets MCP callers distinguish "user
    /// said no" from "we gave up". Issue #H-5 in v0.4.10 review.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// Composite health response. `ready` is true only when every sub-check is
/// healthy; otherwise the response gives the caller enough detail to act on
/// the specific failure (WebView frozen vs. registry overloaded vs. too many
/// child processes, etc.).
#[derive(Serialize)]
struct HealthResponse {
    version: String,
    ready: bool,
    webview: WebviewHealth,
    dialogs: DialogHealth,
    children: ChildrenHealth,
}

#[derive(Serialize)]
struct WebviewHealth {
    /// `true` if the Svelte app answered a `ui:ping` within the timeout.
    responsive: bool,
    /// Round-trip duration in milliseconds; `None` if the ping timed out.
    rtt_ms: Option<u64>,
}

#[derive(Serialize)]
struct DialogHealth {
    /// Currently-pending dialogs in the registry.
    pending: usize,
    /// Age of the oldest pending dialog in seconds; `None` if registry empty.
    oldest_age_secs: Option<u64>,
}

#[derive(Serialize)]
struct ChildrenHealth {
    /// MCP-stdio children currently attached to the lifetime socket.
    attached: usize,
}

/// Wire-contract version (Step 2, cooperative version floor). Bumped ONLY when
/// the HTTP request/response shapes between the bridges and the companion
/// change incompatibly — independent of the app's release version, which moves
/// on every fix. Both bridges read it from `/version` (and `/probe`) and, on a
/// hard mismatch, return a structured "restart this session" tool error instead
/// of being externally killed. Ordinary app-version skew is tolerated as long
/// as `wire_version` matches.
///
/// v1: the original `{spec}` → `{id,cancelled,result,reason}` contract.
pub const WIRE_VERSION: u32 = 1;

#[derive(Serialize)]
struct VersionResponse {
    version: String,
    /// See [`WIRE_VERSION`]. Surfaced so bridges can enforce a cooperative
    /// compatibility floor without anyone killing anyone.
    wire_version: u32,
    build_info: String,
    binary_path: String,
    updater_endpoint: String,
}

#[derive(Serialize)]
struct UpdateResponse {
    updated: bool,
    current: String,
    available: Option<String>,
    error: Option<String>,
    note: Option<String>,
}

pub async fn serve(
    cfg: Arc<AppConfig>,
    dialog: Arc<DialogState>,
    ui_acks: Arc<AckRegistry>,
    lifetime: Arc<LifetimeStats>,
    app: AppHandle,
) -> std::io::Result<()> {
    let port = cfg.http_port;
    let state = AppState {
        cfg,
        dialog,
        ui_acks,
        lifetime,
        app,
        async_slots: Arc::new(Mutex::new(std::collections::HashMap::new())),
    };

    let router = Router::new()
        .route("/health", get(health))
        .route("/render", post(render))
        .route("/render/{id}", get(render_poll))
        .route("/version", get(version))
        .route("/update", post(update))
        .route("/ping", get(ping))
        .route("/probe", get(probe))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = bind_with_reuse(addr)?;
    trace(&format!("serve: listening on {addr}"));
    log::info!("[aiui] http listening on {addr}");
    axum::serve(listener, router)
        .await
        .map_err(std::io::Error::other)?;
    Ok(())
}

/// Bind a TCP listener with `SO_REUSEADDR` (and `SO_REUSEPORT` on macOS)
/// set *before* `bind()`, so a fresh aiui can take the port over a
/// just-exited instance without waiting for the kernel's 30–60s TIME_WAIT
/// window. Without this, every restart-within-a-minute hits "Address
/// already in use" — the dominant cause of the user-perceived "aiui
/// klemmt oft mit Port belegt". Issue #75.
fn bind_with_reuse(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Socket, Type};

    let domain = match addr {
        SocketAddr::V4(_) => Domain::IPV4,
        SocketAddr::V6(_) => Domain::IPV6,
    };
    let socket = Socket::new(domain, Type::STREAM, None)?;
    // SO_REUSEADDR alone is sufficient on macOS to bind over a port that's
    // in TIME_WAIT from a previous listener — Linux's stricter semantics
    // would also need SO_REUSEPORT, but aiui only ships on macOS.
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;

    let std_listener: std::net::TcpListener = socket.into();
    tokio::net::TcpListener::from_std(std_listener)
}

async fn ping() -> &'static str {
    trace("ping: hit");
    "pong"
}

/// Authenticated probe used by the tunnel-manager's shared-forward
/// detection. Unlike /ping, this requires the bearer token, so it
/// distinguishes "another aiui with our token is forwarding the port"
/// from "some random process on :7777 is answering".
///
/// Since 0.4.33: response carries `pid` and `build_sha` so the calling
/// tunnel-manager can verify *its own* aiui is on the other end vs. a
/// concurrent second aiui-app instance with the same token (the case
/// that produced the 2026-05-04 connection-reset incident — two
/// companions, both with the user's token, indistinguishable from
/// `aiui: true` alone).
async fn probe(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !auth_ok(&headers, &state.cfg.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    Json(serde_json::json!({
        "aiui": true,
        "version": env!("CARGO_PKG_VERSION"),
        "wire_version": WIRE_VERSION,
        "pid": std::process::id(),
        "build_sha": env!("AIUI_GIT_SHA"),
    }))
    .into_response()
}

fn auth_ok(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v == token)
        .unwrap_or(false)
}

/// Composite health check. Probes the WebView event loop with a `ui:ping`
/// round-trip, reads live counters from the dialog registry and lifetime
/// tracker, and reports `ready` only when all three are healthy. Computed
/// on-demand — there is no background task maintaining a "current health"
/// state, so an idle companion does no liveness work whatsoever.
async fn health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !auth_ok(&headers, &state.cfg.token) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"unauthorized"})))
            .into_response();
    }

    let webview = probe_webview(&state).await;
    let dialog_stats = state.dialog.stats();
    let attached = state.lifetime.child_count();

    let dialogs = DialogHealth {
        pending: dialog_stats.orphan_count,
        oldest_age_secs: dialog_stats.oldest_age_secs,
    };
    let children = ChildrenHealth { attached };

    // Ready criterion: WebView answers, room left in the dialog
    // registry, and we aren't drowning in attached children.
    //
    // The dialog check uses *strict* less-than because `register()`
    // evicts an existing pending dialog when `len() >= HARD_CAP`. If we
    // reported ready at exactly the cap, the very next /render would
    // silently cancel an in-flight dialog while /health still claimed
    // healthy — readiness must lead the eviction signal, not coincide
    // with it.
    let ready = webview.responsive
        && dialog_stats.orphan_count < crate::dialog::DIALOG_HARD_CAP
        && attached < 32;

    let body = HealthResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        ready,
        webview,
        dialogs,
        children,
    };

    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body)).into_response()
}

/// Round-trip a `ui:ping` event through the frontend and back via the
/// `ui_pong` Tauri command. Returns the observed RTT, or `None` on timeout.
async fn probe_webview(state: &AppState) -> WebviewHealth {
    let (id, rx) = state.ui_acks.register();
    let started = std::time::Instant::now();
    // Probe the dialog window's webview specifically — the setup
    // window is user-driven and irrelevant for render-pipeline health.
    // If no dialog window exists yet, we report `responsive: true`
    // because there's nothing to be unresponsive *about*.
    if state
        .app
        .get_webview_window(crate::DIALOG_WINDOW_LABEL)
        .is_none()
    {
        state.ui_acks.forget(&id);
        return WebviewHealth {
            responsive: true,
            rtt_ms: Some(0),
        };
    }
    if let Err(e) = state
        .app
        .emit_to(crate::DIALOG_WINDOW_LABEL, "ui:ping", &id)
    {
        trace(&format!("health: emit ui:ping failed: {e}"));
        state.ui_acks.forget(&id);
        return WebviewHealth {
            responsive: false,
            rtt_ms: None,
        };
    }
    match tokio::time::timeout(UI_PING_TIMEOUT, rx).await {
        Ok(Ok(())) => WebviewHealth {
            responsive: true,
            rtt_ms: Some(started.elapsed().as_millis() as u64),
        },
        _ => {
            state.ui_acks.forget(&id);
            WebviewHealth {
                responsive: false,
                rtt_ms: None,
            }
        }
    }
}

async fn version(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<VersionResponse>, StatusCode> {
    if !auth_ok(&headers, &state.cfg.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        wire_version: WIRE_VERSION,
        build_info: crate::logging::BUILD_INFO.to_string(),
        binary_path: crate::setup::app_binary_path(),
        updater_endpoint:
            "https://github.com/byte5ai/aiui/releases/latest/download/latest.json".to_string(),
    }))
}

/// Check for an aiui update, download-and-install it if present, and answer
/// the caller *before* scheduling the relaunch. The 500ms delay between
/// returning the response and calling `app.restart()` gives Axum time to
/// finalize the wire response so the MCP client receives `{updated: true,
/// from, to}` even though the process exits shortly after.
async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<UpdateResponse>, (StatusCode, Json<UpdateResponse>)> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    if !auth_ok(&headers, &state.cfg.token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(UpdateResponse {
                updated: false,
                current: current.clone(),
                available: None,
                error: Some("unauthorized".into()),
                note: None,
            }),
        ));
    }

    let updater = match state.app.updater() {
        Ok(u) => u,
        Err(e) => {
            trace(&format!("update: updater unavailable: {e}"));
            return Ok(Json(UpdateResponse {
                updated: false,
                current,
                available: None,
                error: Some(format!("updater unavailable: {e}")),
                note: None,
            }));
        }
    };

    let check = updater.check().await;
    let update = match check {
        Ok(opt) => opt,
        Err(e) => {
            trace(&format!("update: check failed: {e}"));
            return Ok(Json(UpdateResponse {
                updated: false,
                current,
                available: None,
                error: Some(format!("check failed: {e}")),
                note: None,
            }));
        }
    };

    let Some(update) = update else {
        trace("update: already on latest");
        return Ok(Json(UpdateResponse {
            updated: false,
            current,
            available: None,
            error: None,
            note: Some("already on latest".into()),
        }));
    };

    let to_version = update.version.clone();
    trace(&format!("update: installing {current} -> {to_version}"));

    if let Err(e) = update.download_and_install(|_, _| {}, || {}).await {
        trace(&format!("update: install failed: {e}"));
        return Ok(Json(UpdateResponse {
            updated: false,
            current,
            available: Some(to_version),
            error: Some(format!("install failed: {e}")),
            note: None,
        }));
    }

    // Install succeeded. Schedule the relaunch AFTER we've returned this
    // response so the agent receives the version delta. 500ms is plenty for
    // Axum to flush + close the TCP write side before exit.
    //
    // v0.4.43: explicit pre_exit_cleanup before `.restart()`. The
    // Tauri-internal restart path *does* emit RunEvent::ExitRequested
    // (which our `run`-callback catches), so this is belt-and-braces
    // — but a duplicate cleanup is cheap (idempotent sweep) and
    // guarantees the ssh-NTR children die before the new process tries
    // to claim their port. Without this, a previous bug had the
    // restart's tunnels racing the new GUI's startup-sweep.
    let app_handle = state.app.clone();
    let http_port = state.cfg.http_port;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Case (c): latch the single exit authority so the `ExitRequested`
        // default-deny gate honours the restart-initiated exit instead of
        // vetoing it (Invariant I1). `app.restart()` fires ExitRequested.
        if let Some(auth) =
            app_handle.try_state::<std::sync::Arc<crate::lifetime::ExitAuthority>>()
        {
            auth.authorize();
        }
        crate::housekeeping::pre_exit_cleanup(http_port, "updater-restart");
        trace("update: restarting into new binary");
        app_handle.restart();
    });

    Ok(Json(UpdateResponse {
        updated: true,
        current,
        available: Some(to_version),
        error: None,
        note: Some("relaunching into new version".into()),
    }))
}

/// Field `kind`s the dialog frontend knows how to render. A spec
/// carrying anything else would fall through to the "unknown_kind"
/// placeholder in the WebView — exactly the kind of broken/empty
/// surface we now refuse to show. Keep in sync with DialogShell /
/// Form.svelte. (v0.4.46, Bug B+.)
const KNOWN_FIELD_KINDS: &[&str] = &[
    "text", "password", "secret", "number", "select", "checkbox", "slider",
    "date", "datetime", "date_range", "color", "static_text", "markdown",
    "image", "mermaid", "wireframe", "image_grid", "list", "table", "tree",
];

/// Validate a dialog spec *before* any window is created (v0.4.46,
/// Bug B+). On failure returns `(detail, hint)` describing precisely
/// what's wrong; the caller turns that into a structured `invalid_spec`
/// response so the agent can fix the spec and retry — and the user never
/// sees a broken or empty fallback window. Deliberately conservative:
/// only rejects what the frontend genuinely cannot render (bad
/// top-level kind, unknown field kind), never well-formed-but-unusual
/// specs.
fn validate_spec(spec: &serde_json::Value) -> Result<(), (String, String)> {
    let kind = spec.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(kind, "ask" | "form" | "confirm") {
        return Err((
            format!("top-level 'kind' must be one of ask|form|confirm, got '{kind}'"),
            "Use confirm for yes/no, ask for one-of-N, form for ≥2 inputs.".into(),
        ));
    }
    let mut fields: Vec<&serde_json::Value> = Vec::new();
    if let Some(tabs) = spec.get("tabs").and_then(|v| v.as_array()) {
        for t in tabs {
            if let Some(fs) = t.get("fields").and_then(|v| v.as_array()) {
                fields.extend(fs.iter());
            }
        }
    }
    if let Some(fs) = spec.get("fields").and_then(|v| v.as_array()) {
        fields.extend(fs.iter());
    }
    for f in fields {
        let fk = f.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if !KNOWN_FIELD_KINDS.contains(&fk) {
            let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("<unnamed>");
            return Err((
                format!("form field '{name}' has unknown kind '{fk}'"),
                format!("Allowed field kinds: {}.", KNOWN_FIELD_KINDS.join(", ")),
            ));
        }
    }
    Ok(())
}

/// RAII cleanup for a registered render — closes the cancellation-safety hole
/// behind the 409-storm + stranded-empty-window pair (2026-05-30 report).
///
/// `/render` registers a dialog, surfaces a window, then parks on
/// `timeout(DIALOG_TTL=2h, result_rx)`. The MCP client gives up far sooner —
/// the local Rust bridge's reqwest client times out at 300 s — and on any
/// client-side give-up (timeout, ReadError, tunnel blip, slow dialog) Axum
/// **drops this handler future**. None of the explicit teardown below then
/// runs, so the registry entry sits pending for the full 2 h TTL — every
/// subsequent `/render` gets a 409 — and the already-surfaced window is left
/// stranded empty.
///
/// This guard is armed right after `try_register` and runs on *any* drop,
/// including the future-cancelled case the explicit paths can't reach: it
/// cancels the registry entry (freeing the slot immediately) and destroys the
/// dialog window. It is disarmed once the handler completes its own terminal
/// teardown, so the normal paths keep their precise behaviour and we don't
/// double-hop the main thread. `dialog.cancel` is a no-op once the entry is
/// gone and `destroy_dialog_window` is idempotent, so an over-fire is harmless.
///
/// Note: this is a targeted robustness fix, not the spec's Step 3 (async
/// `/render`), which removes the multi-minute held connection entirely. It
/// makes the *current* synchronous handler cancellation-safe in the meantime.
struct RenderGuard {
    id: String,
    dialog: Arc<DialogState>,
    /// `None` only in unit tests, where no Tauri app exists to host a window.
    app: Option<AppHandle>,
    armed: bool,
}

impl RenderGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RenderGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        trace(&format!(
            "render: handler future dropped before terminal teardown — \
             cleaning up id={} (cancel registry entry + destroy window)",
            self.id
        ));
        // Free the registry slot so the next /render isn't 409'd for 2 h.
        self.dialog.cancel(&self.id);
        // Tear down the surfaced window (labelled by id) so it can't strand.
        if let Some(app) = &self.app {
            let app_for_destroy = app.clone();
            let id_for_destroy = self.id.clone();
            let _ = app.run_on_main_thread(move || {
                crate::destroy_dialog_window(&app_for_destroy, &id_for_destroy)
            });
        }
    }
}

/// Await a registered dialog's terminal outcome (bounded by `DIALOG_TTL`),
/// then tear its window down. Shared by the synchronous POST path (awaited
/// inline) and the async path (run in a detached task that fills the
/// `AsyncSlot`). Factoring it out keeps the two paths byte-for-byte identical
/// in resolution + teardown semantics.
async fn resolve_dialog(
    state: AppState,
    id: String,
    result_rx: tokio::sync::oneshot::Receiver<crate::dialog::DialogResult>,
) -> crate::dialog::DialogResult {
    trace(&format!("render: awaiting user response id={}", id));
    let result = match tokio::time::timeout(DIALOG_TTL, result_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => crate::dialog::DialogResult {
            id: id.clone(),
            cancelled: true,
            result: serde_json::Value::Null,
            reason: Some("channel_dropped".into()),
        },
        Err(_) => {
            // TTL expired without user response. Cancel the registry entry
            // (frees its slot) and produce the same 200-OK cancelled shape a
            // user-driven cancel produces — only `reason` differs (#36, the
            // v0.4.45 Bug #5 fix: never surface this as a transport error).
            trace(&format!("render: TTL expired id={}", id));
            state.dialog.cancel(&id);
            crate::dialog::DialogResult {
                id: id.clone(),
                cancelled: true,
                result: serde_json::Value::Null,
                reason: Some("ttl_expired".into()),
            }
        }
    };
    trace(&format!(
        "render: got response id={} cancelled={}",
        result.id, result.cancelled
    ));
    // Authoritative window teardown (v0.4.46, Bug B): single point that
    // guarantees a dialog window never outlives its dialog. Idempotent —
    // a no-op on the submit/cancel paths where the window is already gone.
    let app_for_destroy = state.app.clone();
    let id_for_destroy = id.clone();
    let _ = state
        .app
        .run_on_main_thread(move || crate::destroy_dialog_window(&app_for_destroy, &id_for_destroy));
    result
}

/// Drop async-render result slots older than `DIALOG_TTL` — covers the case
/// where a caller posts an async render, the dialog resolves, but the caller
/// never collects the result via GET (process died after POST). Called
/// opportunistically on each new async render; no background reaper.
fn sweep_async_slots(state: &AppState) {
    let now = Instant::now();
    state
        .async_slots
        .lock()
        .unwrap()
        .retain(|_, s| now.duration_since(s.created_at) <= DIALOG_TTL);
}

/// Outcome of looking up an async-render slot by id.
enum SlotLook {
    /// Resolved — the terminal result (already removed from the map).
    Ready(crate::dialog::DialogResult),
    /// Registered but not yet resolved.
    Pending,
    /// No such id — never an async render, or already collected.
    Gone,
}

/// Drain an async-render slot: if resolved, take its result and remove the slot
/// (`Ready`); if still in flight, `Pending`; if absent, `Gone`. Pure over the
/// map so the `/render/{id}` branching is unit-testable without a Tauri app.
fn drain_async_slot(
    slots: &mut std::collections::HashMap<String, AsyncSlot>,
    id: &str,
) -> SlotLook {
    let taken = match slots.get_mut(id) {
        Some(slot) => slot.result.take(),
        None => return SlotLook::Gone,
    };
    match taken {
        Some(result) => {
            slots.remove(id);
            SlotLook::Ready(result)
        }
        None => SlotLook::Pending,
    }
}

/// GET `/render/{id}` — bounded long-poll for an async render's result (Step
/// 3). Returns the terminal `{id, cancelled, result, reason}` once available
/// (and drains the slot), `{pending: true}` after one `ASYNC_POLL_WINDOW` so
/// the caller re-polls, or 404 for an unknown id (never an async render, or
/// already collected). The caller loops GET until terminal or it gives up.
async fn render_poll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !auth_ok(&headers, &state.cfg.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let deadline = Instant::now() + ASYNC_POLL_WINDOW;
    loop {
        let look = drain_async_slot(&mut state.async_slots.lock().unwrap(), &id);
        match look {
            SlotLook::Ready(result) => {
                trace(&format!("render_poll: delivered id={}", id));
                return Json(RenderResponse {
                    id: result.id,
                    cancelled: result.cancelled,
                    result: result.result,
                    reason: result.reason,
                })
                .into_response();
            }
            SlotLook::Gone => {
                trace(&format!("render_poll: unknown id={}", id));
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "unknown_render_id", "id": id})),
                )
                    .into_response();
            }
            SlotLook::Pending => {
                if Instant::now() >= deadline {
                    return Json(serde_json::json!({"pending": true, "id": id}))
                        .into_response();
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn render(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    trace(&format!("render: entered, body_len={}", body.len()));
    if !auth_ok(&headers, &state.cfg.token) {
        trace("render: auth FAILED");
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"unauthorized"}))).into_response();
    }
    let mut req: RenderRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            trace(&format!("render: body parse failed: {e}"));
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e.to_string()}))).into_response();
        }
    };
    trace(&format!("render: auth ok, spec={}", req.spec));

    // Resolve any http(s):// values in `src` / `thumbnail` fields to
    // `data:` URLs before the spec hits the WebView. The WebView's
    // CSP only permits `data:` for img-src; without this pass an
    // agent's plain URL would silently render as a broken image.
    // See companion/src-tauri/src/imageresolve.rs for failure modes.
    crate::imageresolve::resolve_image_srcs(&mut req.spec).await;

    // Spec validation (v0.4.46, Bug B+): reject anything the frontend
    // can't render *before* creating a window, and tell the agent
    // exactly what to fix. Without this, a bad `kind` opened a window
    // showing the "unknown_kind" placeholder — a confusing surface the
    // user had to dismiss. Now the agent gets `invalid_spec` + detail
    // and can correct the call; nothing is shown to the user.
    if let Err((detail, hint)) = validate_spec(&req.spec) {
        trace(&format!("render: rejected — invalid_spec: {detail}"));
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "invalid_spec",
                "detail": detail,
                "hint": hint,
            })),
        )
            .into_response();
    }

    // Multi-window (Step 4, I8): N dialogs may be in flight at once — the
    // single-occupancy 409 is gone. `register_dialog` stores the request (the
    // per-id window pulls it via `get_dialog_spec`) and only evicts the oldest
    // if the hard cap is hit. Setup-window UI calls don't go through /render,
    // so this governs agent dialog traffic only. Size is estimated from the
    // spec before it moves into the registry.
    let size = crate::dialog::estimate_dialog_size(&req.spec);
    let (id, result_rx) = state.dialog.register_dialog(
        req.spec,
        req.session,
        req.session_origin,
        DIALOG_TTL.as_secs(),
    );
    trace(&format!("render: registered id={}", id));
    // Cancellation-safety net: until the terminal teardown (sync) or the
    // hand-off to the detached task (async), a dropped handler future must not
    // leak the registry entry or strand the window. See `RenderGuard`.
    let mut guard = RenderGuard {
        id: id.clone(),
        dialog: state.dialog.clone(),
        app: Some(state.app.clone()),
        armed: true,
    };

    // Build a fresh window labelled by the dialog id (Step 4 pull model). The
    // window reads its own label and fetches the spec via `get_dialog_spec` on
    // mount — there is no `dialog:show` emit, no ready-handshake, no ack
    // timeout, and no reload-retry, because the frontend initiates and so
    // can't race an event it isn't listening for yet. Window ops are
    // main-thread-only.
    {
        let app_for_build = state.app.clone();
        let id_for_build = id.clone();
        let _ = state.app.run_on_main_thread(move || {
            if let Err(e) = crate::build_dialog_window(&app_for_build, &id_for_build, size) {
                trace(&format!(
                    "render: build_dialog_window failed id={id_for_build}: {e}"
                ));
            }
        });
    }

    // ── Async branch (Step 3) ───────────────────────────────────────────
    // If the caller opted in (header `x-aiui-async`), hand the dialog off to a
    // detached task and answer immediately with `{id, ttl_secs}` (202). The
    // caller polls `GET /render/{id}`. This removes the multi-minute open HTTP
    // connection that a tunnel/GUI blip turns into a remote ReadError —
    // resolution now lives in a task, not on the wire.
    if headers.contains_key(ASYNC_RENDER_HEADER) {
        // The detached task owns resolution + window teardown from here.
        guard.disarm();
        sweep_async_slots(&state);
        {
            let mut slots = state.async_slots.lock().unwrap();
            slots.insert(
                id.clone(),
                AsyncSlot { result: None, created_at: Instant::now() },
            );
        }
        let task_state = state.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            let result = resolve_dialog(task_state.clone(), task_id.clone(), result_rx).await;
            if let Some(slot) = task_state.async_slots.lock().unwrap().get_mut(&task_id) {
                slot.result = Some(result);
            }
        });
        trace(&format!("render: async accepted id={}", id));
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "id": id, "ttl_secs": DIALOG_TTL.as_secs() })),
        )
            .into_response();
    }

    // ── Synchronous path (legacy, backward-compatible) ──────────────────
    // No opt-in header → hold the connection until the user answers, exactly
    // as before. The guard stays armed across the inline await so a dropped
    // connection still cleans up; `resolve_dialog` runs the terminal teardown.
    let result = resolve_dialog(state.clone(), id.clone(), result_rx).await;
    guard.disarm();

    // Lifecycle-driven update check (#42): fire once after every
    // successful render. Frontend gates with a 30-min cooldown so this is
    // never noisier than the old 6h timer in active use, and zero load
    // when nobody is talking to aiui.
    if let Err(e) = state.app.emit("update:check", "post-render") {
        trace(&format!("render: emit update:check failed: {e}"));
    }

    Json(RenderResponse {
        id: result.id,
        cancelled: result.cancelled,
        result: result.result,
        reason: result.reason,
    })
    .into_response()
}

#[cfg(test)]
mod validate_tests {
    use super::validate_spec;
    use serde_json::json;

    #[test]
    fn accepts_confirm() {
        assert!(validate_spec(&json!({"kind":"confirm","title":"ok?"})).is_ok());
    }

    #[test]
    fn accepts_form_with_known_fields() {
        let spec = json!({"kind":"form","fields":[
            {"kind":"text","name":"a"},
            {"kind":"secret","name":"tok"},
            {"kind":"slider","name":"n"}
        ]});
        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn accepts_form_with_tabs() {
        let spec = json!({"kind":"form","tabs":[
            {"label":"T","fields":[{"kind":"checkbox","name":"c"}]}
        ]});
        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn rejects_unknown_top_level_kind() {
        let err = validate_spec(&json!({"kind":"wizard"})).unwrap_err();
        assert!(err.0.contains("wizard"));
        assert!(err.0.contains("ask|form|confirm"));
    }

    #[test]
    fn rejects_missing_top_level_kind() {
        assert!(validate_spec(&json!({"title":"x"})).is_err());
    }

    #[test]
    fn rejects_unknown_field_kind_with_name() {
        let spec = json!({"kind":"form","fields":[{"kind":"hologram","name":"h"}]});
        let err = validate_spec(&spec).unwrap_err();
        assert!(err.0.contains("'h'"), "detail names the field: {}", err.0);
        assert!(err.0.contains("hologram"));
    }

    #[test]
    fn rejects_unknown_field_kind_in_tab() {
        let spec = json!({"kind":"form","tabs":[
            {"label":"T","fields":[{"kind":"warp","name":"w"}]}
        ]});
        assert!(validate_spec(&spec).is_err());
    }
}

#[cfg(test)]
mod render_guard_tests {
    use super::RenderGuard;
    use crate::dialog::DialogState;
    use std::sync::Arc;

    // Regression: the 409-storm + stranded-empty-window pair (2026-05-30).
    // When the /render handler future is dropped (client give-up) the registry
    // entry must be freed immediately, not left pending for the 2 h TTL. The
    // window-destroy half needs a Tauri app, so these cover the registry half
    // (`app: None`) — the half that produces the 409.

    fn reg(ds: &DialogState) -> (String, tokio::sync::oneshot::Receiver<crate::dialog::DialogResult>) {
        ds.register_dialog(serde_json::json!({"kind": "confirm"}), None, None, 0)
    }

    #[test]
    fn armed_guard_drop_frees_registry_slot() {
        let ds = Arc::new(DialogState::new());
        let (id, result_rx) = reg(&ds);
        assert_eq!(ds.stats().orphan_count, 1);
        {
            let _guard = RenderGuard {
                id: id.clone(),
                dialog: ds.clone(),
                app: None,
                armed: true,
            };
            // future "dropped" here
        }
        // Slot freed → a later render isn't blocked behind a leaked entry.
        assert_eq!(ds.stats().orphan_count, 0);
        // The awaiter observes a cancelled terminal result, not a hang.
        let r = result_rx.blocking_recv().expect("result_tx sent on cancel");
        assert!(r.cancelled);
    }

    #[test]
    fn disarmed_guard_drop_leaves_terminal_path_untouched() {
        // The normal terminal path disarms after its own teardown; the guard
        // must then do nothing (no double-cancel, no spurious slot churn).
        let ds = Arc::new(DialogState::new());
        let (id, _result_rx) = reg(&ds);
        {
            let mut guard = RenderGuard {
                id: id.clone(),
                dialog: ds.clone(),
                app: None,
                armed: true,
            };
            guard.disarm();
        }
        // Entry untouched by the disarmed guard (the real handler's explicit
        // `complete`/`cancel` owns removal on the terminal path).
        assert_eq!(ds.stats().orphan_count, 1);
    }
}

#[cfg(test)]
mod async_render_tests {
    use super::{drain_async_slot, AsyncSlot, SlotLook};
    use std::collections::HashMap;
    use std::time::Instant;

    // Step 3: the GET /render/{id} branching — pending → ready (drained once)
    // → gone — without a Tauri app.
    #[test]
    fn slot_lifecycle_pending_ready_gone() {
        let mut slots: HashMap<String, AsyncSlot> = HashMap::new();
        slots.insert(
            "x".into(),
            AsyncSlot { result: None, created_at: Instant::now() },
        );

        // Registered, not resolved → Pending.
        assert!(matches!(drain_async_slot(&mut slots, "x"), SlotLook::Pending));
        // Unknown id → Gone.
        assert!(matches!(drain_async_slot(&mut slots, "nope"), SlotLook::Gone));

        // Resolve it.
        slots.get_mut("x").unwrap().result = Some(crate::dialog::DialogResult {
            id: "x".into(),
            cancelled: true,
            result: serde_json::Value::Null,
            reason: Some("window_closed".into()),
        });

        // First drain delivers the terminal result.
        match drain_async_slot(&mut slots, "x") {
            SlotLook::Ready(r) => {
                assert!(r.cancelled);
                assert_eq!(r.reason.as_deref(), Some("window_closed"));
            }
            _ => panic!("expected Ready"),
        }
        // Slot was removed → a second drain is Gone (no double-delivery).
        assert!(matches!(drain_async_slot(&mut slots, "x"), SlotLook::Gone));
        assert!(slots.is_empty());
    }
}
