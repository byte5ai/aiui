use crate::config::AppConfig;
use crate::dialog::{DialogRequest, DialogState};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use crate::logging::trace;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

#[derive(Clone)]
struct AppState {
    cfg: Arc<AppConfig>,
    dialog: Arc<DialogState>,
    app: AppHandle,
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

#[derive(Serialize)]
struct HealthResponse {
    version: String,
    ready: bool,
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
    app: AppHandle,
) -> std::io::Result<()> {
    let port = cfg.http_port;
    let state = AppState { cfg, dialog, app };

    let router = Router::new()
        .route("/health", get(health))
        .route("/render", post(render))
        .route("/version", get(version))
        .route("/update", post(update))
        .route("/ping", get(ping))
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

fn auth_ok(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v == token)
        .unwrap_or(false)
}

async fn health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<HealthResponse>, StatusCode> {
    if !auth_ok(&headers, &state.cfg.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(HealthResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        ready: true,
    }))
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

    let (id, rx) = state.dialog.register();
    trace(&format!("render: registered id={}", id));
    let dr = DialogRequest {
        id: id.clone(),
        spec: req.spec,
    };

    // Surface the window from the main thread
    let app_for_show = state.app.clone();
    let id_for_log = id.clone();
    let rc = app_for_show.clone().run_on_main_thread(move || {
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

    // Emit the dialog to the frontend
    match state.app.emit("dialog:show", &dr) {
        Ok(_) => trace(&format!("render: emitted dialog:show id={}", id)),
        Err(e) => trace(&format!("render: emit FAILED: {e}")),
    }

    trace(&format!("render: awaiting user response id={}", id));
    // Wait for user to submit or cancel
    let result = match rx.await {
        Ok(r) => r,
        Err(_) => crate::dialog::DialogResult {
            id: id.clone(),
            cancelled: true,
            result: serde_json::Value::Null,
        },
    };
    trace(&format!(
        "render: got response id={} cancelled={}",
        result.id, result.cancelled
    ));

    Json(RenderResponse {
        id: result.id,
        cancelled: result.cancelled,
        result: result.result,
    })
    .into_response()
}
