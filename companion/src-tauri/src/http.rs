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

#[derive(Serialize)]
struct VersionResponse {
    version: String,
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
    let listener = tokio::net::TcpListener::bind(addr).await?;
    trace(&format!("serve: listening on {addr}"));
    log::info!("[aiui] http listening on {addr}");
    axum::serve(listener, router)
        .await
        .map_err(std::io::Error::other)?;
    Ok(())
}

async fn ping() -> &'static str {
    trace("ping: hit");
    "pong"
}

/// Authenticated probe used by the tunnel-manager's shared-forward
/// detection. Unlike /ping, this requires the bearer token, so it
/// distinguishes "another aiui with our token is forwarding the port"
/// from "some random process on :7777 is answering". Without this
/// distinction a malicious squatter could mask a port-takeover by
/// answering "pong" and aiui would keep showing connected-shared.
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
    if let Err(e) = state.app.emit("ui:ping", &id) {
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
    let app_handle = state.app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
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
    let req: RenderRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            trace(&format!("render: body parse failed: {e}"));
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e.to_string()}))).into_response();
        }
    };
    trace(&format!("render: auth ok, spec={}", req.spec));

    let (id, result_rx, ack_rx) = state.dialog.register();
    trace(&format!("render: registered id={}", id));
    let dr = DialogRequest {
        id: id.clone(),
        spec: req.spec,
    };

    // ── Idle-restart check (#41) ────────────────────────────────────────
    // If the GUI has been up for a long time and the last render was a
    // while ago, reload the WebView before serving this one. Catches
    // accumulated drift (sleep/wake artefacts, stuck event listeners)
    // *exactly* when it would matter — not on a wall-clock timer.
    {
        let last = *state.last_render_at.lock().unwrap();
        if state.started_at.elapsed() > IDLE_RESTART_UPTIME
            && last.elapsed() > IDLE_RESTART_QUIET
        {
            trace(&format!(
                "render: idle-restart trigger (uptime {:?}, last_render {:?} ago)",
                state.started_at.elapsed(),
                last.elapsed()
            ));
            reload_main_webview(&state.app);
            tokio::time::sleep(RELOAD_SETTLE).await;
        }
    }

    // Mark this render attempt — done early so the ack/recreate path
    // still resets the idle clock even if the user closes the dialog.
    *state.last_render_at.lock().unwrap() = Instant::now();

    // Surface the window from the main thread.
    surface_main_window(&state.app, &id);

    // Emit the dialog to the frontend.
    if let Err(e) = state.app.emit("dialog:show", &dr) {
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
            reload_main_webview(&state.app);
            tokio::time::sleep(RELOAD_SETTLE).await;

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
            if let Err(e) = state.app.emit("ui:ping", &probe_id) {
                trace(&format!("render: post-reload ui:ping emit failed: {e}"));
                state.ui_acks.forget(&probe_id);
            }
            match tokio::time::timeout(DIALOG_ACK_TIMEOUT, probe_rx).await {
                Ok(Ok(())) => {
                    trace("render: post-reload webview is responsive, re-emitting dialog:show");
                    if let Err(e) = state.app.emit("dialog:show", &dr) {
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
        },
        Err(_) => {
            // TTL expired without user response. Cancel the registry
            // entry (also frees its slot) and return a structured timeout.
            trace(&format!("render: TTL expired id={}", id));
            state.dialog.cancel(&id);
            return (
                StatusCode::REQUEST_TIMEOUT,
                Json(serde_json::json!({
                    "id": id,
                    "cancelled": true,
                    "error": "timeout",
                    "detail": format!("no user response within {:?}", DIALOG_TTL),
                })),
            )
                .into_response();
        }
    };
    trace(&format!(
        "render: got response id={} cancelled={}",
        result.id, result.cancelled
    ));

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
    })
    .into_response()
}

/// Bring the main webview window to the front from any thread. All Tauri
/// window operations have to run on the main thread, so we hop there via
/// `run_on_main_thread`.
fn surface_main_window(app: &AppHandle, id: &str) {
    let app_for_show = app.clone();
    let id_for_log = id.to_string();
    let rc = app.clone().run_on_main_thread(move || {
        trace(&format!("render: main-thread callback id={}", id_for_log));
        if let Some(win) = app_for_show.get_webview_window("main") {
            trace("render: main-thread got window, calling show()");
            let _ = win.show();
            let _ = win.set_focus();
            let _ = win.unminimize();
            trace("render: main-thread show() returned");
        } else {
            trace("render: main-thread window MISSING");
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
        if let Some(win) = app_for_reload.get_webview_window("main") {
            trace("render: reloading main webview");
            if let Err(e) = win.eval("location.reload()") {
                trace(&format!("render: reload eval failed: {e}"));
            }
        } else {
            trace("render: reload requested but main window is MISSING");
        }
    });
}
