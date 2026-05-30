use crate::ack::AckRegistry;
use crate::config::AppConfig;
use crate::dialog::{DialogRequest, DialogState, DIALOG_TTL};
use crate::lifetime::LifetimeStats;
use axum::{
    extract::State,
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

/// How long the `/render` handler waits for the frontend to acknowledge
/// receipt of `dialog:show` before concluding the WebView event loop is
/// dead and triggering a reload.
const DIALOG_ACK_TIMEOUT: Duration = Duration::from_millis(500);

/// Pause after `webview.reload()` before re-emitting `dialog:show`. Gives
/// the freshly-loaded Svelte app time to mount and register its listener.
const RELOAD_SETTLE: Duration = Duration::from_millis(300);

/// How long `/health` waits for a `ui:ping` round-trip from the frontend
/// before concluding the WebView is unresponsive.
const UI_PING_TIMEOUT: Duration = Duration::from_millis(100);

/// Idle-restart trigger: if the GUI has been alive longer than this AND
/// hasn't served a render recently (see `IDLE_RESTART_QUIET`), the next
/// render reloads the WebView before showing — flushes any drift that
/// accumulated while nobody was watching.
const IDLE_RESTART_UPTIME: Duration = Duration::from_secs(24 * 60 * 60);

/// Minimum time between renders for the long-uptime reload to trigger.
/// Prevents reloading mid-burst when many renders fire close together.
const IDLE_RESTART_QUIET: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
struct AppState {
    cfg: Arc<AppConfig>,
    dialog: Arc<DialogState>,
    ui_acks: Arc<AckRegistry>,
    lifetime: Arc<LifetimeStats>,
    app: AppHandle,
    /// Process-start timestamp for the GUI. Used to evaluate the
    /// idle-restart condition without requiring an OS sleep/wake hook.
    started_at: Instant,
    /// Last time `/render` produced (or attempted to produce) a dialog.
    /// Mutex<Instant> is fine here — contention is bounded by the rate of
    /// /render calls.
    last_render_at: Arc<Mutex<Instant>>,
}

#[derive(Deserialize)]
struct RenderRequest {
    #[serde(default)]
    _timeout_s: Option<u64>,
    spec: serde_json::Value,
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
    let now = Instant::now();
    let state = AppState {
        cfg,
        dialog,
        ui_acks,
        lifetime,
        app,
        started_at: now,
        last_render_at: Arc::new(Mutex::new(now)),
    };

    let router = Router::new()
        .route("/health", get(health))
        .route("/render", post(render))
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
        // Tear down the surfaced window so it can't strand empty.
        if let Some(app) = &self.app {
            let app_for_destroy = app.clone();
            let _ = app.run_on_main_thread(move || {
                crate::destroy_dialog_window(&app_for_destroy)
            });
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

    // v0.4.36: try_register rejects when a dialog is already in flight
    // instead of evicting the existing one. Two parallel callers — multi-
    // call-per-turn, two Claude sessions, or a stale window from a prior
    // timeout — would otherwise overlay each other in the single dialog
    // window, with the older request's `oneshot` resolving as `evicted`
    // exactly while the user was still looking at it. The 409 response
    // gives the second caller a structured "busy" answer so the agent
    // can choose to retry or tell the user the dialog is held by
    // something else. Setup-window-driven UI calls don't go through
    // /render at all, so this only governs agent dialog traffic.
    let (id, result_rx, ack_rx) = match state.dialog.try_register() {
        Ok(triple) => triple,
        Err(busy) => {
            trace(&format!(
                "render: rejected — companion busy (pending={}, oldest_age={}s)",
                busy.pending_count, busy.oldest_age_secs
            ));
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "busy",
                    "pending_count": busy.pending_count,
                    "oldest_age_secs": busy.oldest_age_secs,
                })),
            )
                .into_response();
        }
    };
    trace(&format!("render: registered id={}", id));
    // Cancellation-safety net: from here until the explicit terminal teardown
    // below, a dropped handler future (client give-up) must not leak the
    // registry entry or strand the window. See `RenderGuard`.
    let mut guard = RenderGuard {
        id: id.clone(),
        dialog: state.dialog.clone(),
        app: Some(state.app.clone()),
        armed: true,
    };
    let dr = DialogRequest {
        id: id.clone(),
        spec: req.spec,
        // Sent so the frontend can schedule warning banners + auto-cancel
        // a fraction before the backend sweep fires. Single source of
        // truth lives in `DIALOG_TTL`. v0.4.41.
        ttl_secs: DIALOG_TTL.as_secs(),
    };

    // ── Idle-restart check (#41) ────────────────────────────────────────
    // If the GUI has been up for a long time and the last render was a
    // while ago, reload the WebView before serving this one. Catches
    // accumulated drift (sleep/wake artefacts, stuck event listeners)
    // *exactly* when it would matter — not on a wall-clock timer.
    //
    // Important: never reload while a previous dialog is still pending.
    // The reload tears down the WebView's JS state including any active
    // dialog the user might be looking at, and the still-awaiting
    // `/render` handler would get a `channel_dropped` cancellation
    // instead of the user's actual answer. Only reload when the registry
    // is empty. Issue #H-6 in v0.4.10 review.
    {
        let last = *state.last_render_at.lock().unwrap();
        let pending = state.dialog.stats().orphan_count;
        if state.started_at.elapsed() > IDLE_RESTART_UPTIME
            && last.elapsed() > IDLE_RESTART_QUIET
            && pending == 0
        {
            trace(&format!(
                "render: idle-restart trigger (uptime {:?}, last_render {:?} ago, registry empty)",
                state.started_at.elapsed(),
                last.elapsed()
            ));
            reload_main_webview(&state.app);
            tokio::time::sleep(RELOAD_SETTLE).await;
        } else if state.started_at.elapsed() > IDLE_RESTART_UPTIME
            && last.elapsed() > IDLE_RESTART_QUIET
        {
            trace(&format!(
                "render: idle-restart suppressed — {} pending dialog(s) in registry",
                pending
            ));
        }
    }

    // Mark this render attempt — done early so the ack/recreate path
    // still resets the idle clock even if the user closes the dialog.
    *state.last_render_at.lock().unwrap() = Instant::now();

    // Surface the window from the main thread. If the window is being
    // built fresh (first render of this session, or after the user
    // closed it), `ensure_dialog_window` reset the ready flag.
    // Window-size estimate is per-spec — wide widgets widen, long
    // forms grow vertically. v0.4.40.
    let size = crate::dialog::estimate_dialog_size(&dr.spec);
    surface_main_window(&state.app, &id, size);

    // Window-ready handshake: wait until the frontend signals that
    // its `dialog:show` listener is registered. Without this gate
    // we'd race against Vite-bundle-load + Svelte-mount + tauri-listen,
    // and on the first render of a session the emit would land before
    // the listener — silent loss, 500 ms ack timeout, webview reload
    // and a confused user staring at a blank window.
    wait_for_dialog_ready(&state.app, "pre-emit").await;

    // Emit the dialog to the frontend.
    if let Err(e) = state
        .app
        .emit_to(crate::DIALOG_WINDOW_LABEL, "dialog:show", &dr)
    {
        trace(&format!("render: emit FAILED: {e}"));
    } else {
        trace(&format!("render: emitted dialog:show id={}", id));
    }

    // ── Ack-Contract ────────────────────────────────────────────────────
    // Wait briefly for the frontend to confirm receipt. If no ack arrives,
    // the WebView event loop is most likely dead — try to revive it by
    // reloading the webview, then re-emitting once. If the second ack also
    // fails, give up and surface a structured error to the caller instead
    // of blocking indefinitely on a dialog the user will never see.
    match tokio::time::timeout(DIALOG_ACK_TIMEOUT, ack_rx).await {
        Ok(Ok(())) => {
            trace(&format!("render: ack ok id={}", id));
        }
        _ => {
            trace(&format!(
                "render: no ack within {:?}; reloading webview and retrying",
                DIALOG_ACK_TIMEOUT
            ));
            // Reset ready flag — after reload the listeners need to
            // re-register. We'll wait on the handshake again before
            // re-emitting.
            if let Some(tx) = state
                .app
                .try_state::<std::sync::Arc<tokio::sync::watch::Sender<bool>>>()
            {
                let _ = tx.inner().send(false);
            }
            reload_main_webview(&state.app);
            tokio::time::sleep(RELOAD_SETTLE).await;

            // Wait for the freshly-mounted Svelte to signal listeners
            // are wired up again. Without this the same race that got
            // us here would just repeat after reload.
            wait_for_dialog_ready(&state.app, "post-reload").await;

            // After reload the previous ack receiver was consumed. We need a
            // fresh handshake on the same dialog id — register a new ack
            // slot tied to the same id is overkill; instead we just re-emit
            // and wait on the same (already-armed) ack registry by treating
            // the second emit's resolution as the ack we care about.
            //
            // Since `register()` only created one ack channel and we just
            // consumed its receiver via the timeout, we have to fall back
            // to a small generic ack via the AckRegistry for the second
            // round. That keeps DialogState simple.
            let (probe_id, probe_rx) = state.ui_acks.register();
            if let Err(e) = state
                .app
                .emit_to(crate::DIALOG_WINDOW_LABEL, "ui:ping", &probe_id)
            {
                trace(&format!("render: post-reload ui:ping emit failed: {e}"));
                state.ui_acks.forget(&probe_id);
            }
            match tokio::time::timeout(DIALOG_ACK_TIMEOUT, probe_rx).await {
                Ok(Ok(())) => {
                    trace("render: post-reload webview is responsive, re-emitting dialog:show");
                    if let Err(e) =
                        state.app.emit_to(crate::DIALOG_WINDOW_LABEL, "dialog:show", &dr)
                    {
                        trace(&format!("render: re-emit FAILED: {e}"));
                    }
                }
                _ => {
                    state.ui_acks.forget(&probe_id);
                    trace("render: webview still unreachable after reload — giving up");
                    state.dialog.cancel(&id);
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": "ui_unreachable",
                            "detail": "webview did not acknowledge dialog:show after reload",
                        })),
                    )
                        .into_response();
                }
            }
        }
    }

    // ── Normal path ─────────────────────────────────────────────────────
    // Wait for the user's submit/cancel — but bounded by `DIALOG_TTL`. A
    // dialog that nobody answers eventually returns a structured timeout
    // instead of blocking the caller indefinitely (#36). The same TTL is
    // used by the registry's opportunistic sweep, so a timed-out entry
    // gets cancelled regardless of whether this awaiter or the next
    // `register()` call notices first.
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
            // TTL expired without user response. Cancel the registry
            // entry (frees its slot) and fall through to the normal
            // 200-OK response below with cancelled:true + reason.
            //
            // v0.4.45 (Bug #5): previously this returned HTTP 408, which
            // mcp.rs's render_dialog treated as a non-success status →
            // generic "aiui tool error: render http 408" — a different
            // shape than a user-driven cancel (200 {cancelled:true}).
            // The agent then saw a transport error instead of a clean
            // "user didn't respond" cancellation. Now both the
            // user-cancel and the TTL-expiry paths produce the exact
            // same tool-result shape; only `reason` differs.
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

    // Authoritative teardown (v0.4.46, Bug B): the render has reached a
    // terminal outcome — user submit/cancel, native X-close, TTL expiry,
    // or channel-drop. Destroy the dialog window now, from Rust, on the
    // main thread. This is the single point that guarantees a dialog
    // window never outlives its dialog: it covers the TTL/channel-drop
    // paths the frontend's own close never reaches (the empty-window
    // stranding of 2026-05-29), and is a harmless no-op on the
    // submit/cancel paths where the window is already gone.
    {
        let app_for_destroy = state.app.clone();
        let _ = state
            .app
            .run_on_main_thread(move || crate::destroy_dialog_window(&app_for_destroy));
    }
    // Terminal teardown done explicitly above — stand the guard down so it
    // doesn't redundantly re-cancel/re-destroy on scope exit.
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

/// Wait until the dialog window's frontend signals via the
/// `dialog_window_ready` Tauri command that its `dialog:show` and
/// `ui:ping` listeners are registered. Times out after
/// `DIALOG_READY_TIMEOUT` and returns either way — the caller still
/// emits, falling back to the existing ack/reload contract if the
/// frontend turns out to be slower than expected.
///
/// Called twice in the render path: once before the initial emit
/// (covers the cold-start race when the window is built fresh), once
/// after a webview reload (covers the same race after the recovery
/// path tears down the JS state).
const DIALOG_READY_TIMEOUT: Duration = Duration::from_millis(3000);

async fn wait_for_dialog_ready(app: &AppHandle, phase: &str) {
    let Some(tx_state) = app.try_state::<std::sync::Arc<tokio::sync::watch::Sender<bool>>>()
    else {
        trace(&format!("render: dialog_ready_tx state missing ({phase})"));
        return;
    };
    let mut rx = tx_state.inner().subscribe();
    if *rx.borrow() {
        trace(&format!("render: dialog already ready ({phase})"));
        return;
    }
    let started = std::time::Instant::now();
    let waited = tokio::time::timeout(DIALOG_READY_TIMEOUT, async {
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
    if waited.is_ok() && *rx.borrow() {
        trace(&format!(
            "render: dialog ready ({phase}) after {:?}",
            started.elapsed()
        ));
    } else {
        trace(&format!(
            "render: dialog-ready timeout ({phase}) after {:?} — proceeding anyway",
            started.elapsed()
        ));
    }
}

/// Surface the dialog window for the incoming render. If the window
/// already exists, show + focus + unminimize + resize to fit this
/// spec; otherwise build it at the spec-derived inner size.
/// All Tauri window operations have to run on the main thread, so we
/// hop there via `run_on_main_thread`.
fn surface_main_window(app: &AppHandle, id: &str, size: (f64, f64)) {
    let app_for_show = app.clone();
    let id_for_log = id.to_string();
    let rc = app.clone().run_on_main_thread(move || {
        trace(&format!(
            "render: main-thread callback id={} size=({:.0},{:.0})",
            id_for_log, size.0, size.1
        ));
        match crate::ensure_dialog_window(&app_for_show, size) {
            Ok(_win) => {
                trace("render: main-thread dialog window ready (show/build)");
            }
            Err(e) => {
                trace(&format!("render: main-thread dialog window FAILED: {e}"));
            }
        }
    });
    trace(&format!("render: run_on_main_thread returned {:?}", rc.is_ok()));
}

/// Reload the main webview to recover from a stuck JS event loop. Tears
/// down the JS side (DOM, listeners, setIntervals) and re-runs the Svelte
/// app from scratch — Tauri's `webview.reload()` is exactly this. We use
/// it as the recreate path because it's lighter than destroying and
/// rebuilding the window via `WebviewWindowBuilder` and recovers from the
/// same class of failure.
fn reload_main_webview(app: &AppHandle) {
    let app_for_reload = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(win) = app_for_reload.get_webview_window(crate::DIALOG_WINDOW_LABEL) {
            trace("render: reloading dialog webview");
            if let Err(e) = win.eval("location.reload()") {
                trace(&format!("render: reload eval failed: {e}"));
            }
        } else {
            trace("render: reload requested but main window is MISSING");
        }
    });
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

    #[test]
    fn armed_guard_drop_frees_registry_slot() {
        let ds = Arc::new(DialogState::new());
        let (id, result_rx, _ack) = ds.try_register().expect("first register is free");
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
        // Slot freed → the next render would NOT get a 409.
        assert_eq!(ds.stats().orphan_count, 0);
        assert!(ds.try_register().is_ok(), "registry is free again after guard cleanup");
        // The awaiter observes a cancelled terminal result, not a hang.
        let r = result_rx.blocking_recv().expect("result_tx sent on cancel");
        assert!(r.cancelled);
    }

    #[test]
    fn disarmed_guard_drop_leaves_terminal_path_untouched() {
        // The normal terminal path disarms after its own teardown; the guard
        // must then do nothing (no double-cancel, no spurious slot churn).
        let ds = Arc::new(DialogState::new());
        let (id, _result_rx, _ack) = ds.try_register().unwrap();
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
