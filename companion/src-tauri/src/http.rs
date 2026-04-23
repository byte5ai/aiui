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
use crate::logging::trace;
use tauri::{AppHandle, Emitter, Manager};

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
        .route("/ping", get(ping))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    trace(&format!("serve: listening on {addr}"));
    log::info!("[aiui] http listening on {addr}");
    axum::serve(listener, router)
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
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
