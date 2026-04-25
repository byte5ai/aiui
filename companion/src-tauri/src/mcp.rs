//! aiui MCP stdio server (native Rust).
//!
//! Exposes confirm/ask/form/aiui_health/update/version tools plus the
//! widgets/update/version prompts over the MCP JSON-RPC protocol. Dialog
//! rendering is forwarded over HTTP to the GUI companion on
//! localhost:<http_port>; the updater runs inside the companion process via
//! `UpdaterExt`.
//!
//! This server replaces the Python `aiui-mcp` PyPI package for the common
//! case of "aiui.app is installed on the same Mac". Claude Code's
//! `~/.claude.json` points directly at this binary with `--mcp-stdio`, so
//! there is no `uv`/`uvx`/`pipx` dependency on the onboarding path.
//!
//! The Python package stays on PyPI for remote/headless scenarios where
//! aiui.app isn't installed locally (typically SSH targets).

use crate::config::AppConfig;
use crate::logging::trace;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SKILL_MD: &str = include_str!("../../../docs/skill.md");

const UPDATE_PROMPT: &str = "\
Check whether an aiui update is available and install it if so. Call the \
`update` tool now, then report back concisely:

- If `updated: true`, report \"aiui updated {current} -> {available}\" and \
  mention that aiui will relaunch itself silently; the next agent call \
  will hit the new version.
- If `updated: false` and `note: \"already on latest\"`, report \"aiui is \
  on the latest version ({current})\".
- If `error` is set, report the error verbatim.

Keep the reply to one short sentence unless the user asked for detail.
";

const VERSION_PROMPT: &str = "\
Report the current aiui version to the user. Call the `version` tool and \
reply with one short line containing the version plus the build date \
parsed from `build_info` (format \"v{ver} (commit, yyyy-mm-dd)\"). If the \
user asked for more, include the binary path and updater endpoint.
";

/// How long mcp-stdio waits for *any* incoming line before assuming the
/// parent process has gone silent and exiting. This is an event-driven
/// deadline that resets on activity — equivalent to "no input for 6 h ⇒
/// exit", with zero idle cost. Catches the failure mode where Claude
/// Desktop forgets to close our stdin pipe but also never sends another
/// request, which is how stale mcp-stdio children accumulated in the
/// 2026-04-25 incident.
const STDIN_IDLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Top-level entry: read JSON-RPC messages from stdin, dispatch to handlers,
/// write responses to stdout. Runs until stdin closes or the idle deadline
/// expires.
pub async fn run_stdio(cfg: Arc<AppConfig>) {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("reqwest client");

    trace("mcp-stdio: run_stdio entered");

    loop {
        let line = tokio::select! {
            res = reader.next_line() => match res {
                Ok(Some(l)) => l,
                Ok(None) => {
                    trace("mcp-stdio: stdin closed, exiting");
                    return;
                }
                Err(e) => {
                    trace(&format!("mcp-stdio: stdin error: {e}, exiting"));
                    return;
                }
            },
            _ = tokio::time::sleep(STDIN_IDLE_LIMIT) => {
                trace(&format!(
                    "mcp-stdio: no input for {:?}, parent likely gone, exiting",
                    STDIN_IDLE_LIMIT
                ));
                return;
            }
        };

        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id_opt = msg.get("id").cloned();
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // Notifications (no id) — we only care about "initialized"; everything
        // else is silently dropped per JSON-RPC spec.
        let Some(id) = id_opt else {
            continue;
        };

        let response = match dispatch(method, params, &cfg, &http).await {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": err.code, "message": err.message }
            }),
        };
        let _ = stdout
            .write_all(format!("{response}\n").as_bytes())
            .await;
        let _ = stdout.flush().await;
    }
}

struct RpcError {
    code: i64,
    message: String,
}

async fn dispatch(
    method: &str,
    params: Value,
    cfg: &Arc<AppConfig>,
    http: &reqwest::Client,
) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {
                "tools": {},
                "prompts": {}
            },
            "serverInfo": {
                "name": "aiui",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "tools/list" => Ok(json!({ "tools": tools_list() })),
        "tools/call" => tools_call(params, cfg, http).await,
        "prompts/list" => Ok(json!({ "prompts": prompts_list() })),
        "prompts/get" => prompts_get(params),
        _ => Err(RpcError {
            code: -32601,
            message: format!("method not found: {method}"),
        }),
    }
}

// ---------- tools ----------

fn tools_list() -> Value {
    json!([
        {
            "name": "confirm",
            "description": "USE WHEN you would otherwise ask the user a yes/no question in chat — and ALWAYS before any irreversible step (delete, drop, force-push, rollback, prod deploy). Renders a native macOS yes/no window; pass `destructive: true` for a red confirm button on dangerous actions. Returns {cancelled, confirmed}. For 3+ options, use `ask`. For information the user only reads, render in chat.",
            "inputSchema": {
                "type": "object",
                "required": ["title"],
                "properties": {
                    "title": { "type": "string", "description": "Decision as a question, ≤ 10 words." },
                    "message": { "type": "string", "description": "One sentence stating the concrete consequence." },
                    "header": { "type": "string", "description": "Short chip above the title (≤ 14 chars)." },
                    "destructive": { "type": "boolean", "default": false, "description": "Red confirm button — for deletions/rollbacks only." },
                    "confirm_label": { "type": "string" },
                    "cancel_label": { "type": "string" }
                }
            }
        },
        {
            "name": "ask",
            "description": "USE WHEN you would otherwise list options in chat and wait for the user to type back which one — picking a deploy strategy, a migration path, a file to act on, etc. Renders a native macOS choice window with per-option descriptions, optional multi-select and free-text fallback. Returns {cancelled, answers, other?}. For yes/no, use `confirm`. For ≥ 2 related inputs, use `form`.",
            "inputSchema": {
                "type": "object",
                "required": ["question", "options"],
                "properties": {
                    "question": { "type": "string", "description": "Full question, imperative or interrogative." },
                    "options": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string" },
                                "description": { "type": "string" },
                                "value": { "type": "string" }
                            },
                            "required": ["label"]
                        }
                    },
                    "header": { "type": "string" },
                    "multi_select": { "type": "boolean", "default": false },
                    "allow_other": { "type": "boolean", "default": false }
                }
            }
        },
        {
            "name": "form",
            "description": "USE WHEN the user needs to give you ≥ 2 related inputs, or any single input that's better entered somewhere other than the chat — secrets (password field, masked on screen), dates (datetime/range), bounded numbers (slider), sortable rankings, multi-selects, color picks. Renders a native macOS form window. Fields: text, password, number, select, checkbox, slider, date, date_range, color, static_text, list, tree. Footer actions support primary (blue), success (green), destructive (red). Returns {cancelled, action?, values}. For yes/no, use `confirm`. For one option pick, use `ask`.",
            "inputSchema": {
                "type": "object",
                "required": ["title", "fields"],
                "properties": {
                    "title": { "type": "string" },
                    "fields": { "type": "array", "items": { "type": "object" } },
                    "description": { "type": "string" },
                    "header": { "type": "string" },
                    "actions": { "type": "array", "items": { "type": "object" } },
                    "submit_label": { "type": "string" },
                    "cancel_label": { "type": "string" }
                }
            }
        },
        {
            "name": "aiui_health",
            "description": "Reachability check against the local aiui companion. Returns version + ready flag if the companion is running and responding.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "version",
            "description": "Report aiui companion version, build info, binary path, and the updater endpoint. Cheap; does not hit the network.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "update",
            "description": "Check for an aiui update, download-and-install if one is available, then relaunch silently. Responds BEFORE the relaunch so the caller receives {updated, current, available, note}. Next agent call hits the new version.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

async fn tools_call(
    params: Value,
    cfg: &Arc<AppConfig>,
    http: &reqwest::Client,
) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let outcome = match name.as_str() {
        "confirm" => render_dialog(
            json!({
                "kind": "confirm",
                "title": args.get("title"),
                "message": args.get("message"),
                "header": args.get("header"),
                "destructive": args.get("destructive").and_then(|v| v.as_bool()).unwrap_or(false),
                "confirmLabel": args.get("confirm_label"),
                "cancelLabel": args.get("cancel_label")
            }),
            cfg,
            http,
        )
        .await
        .map(format_confirm_result),

        "ask" => render_dialog(
            json!({
                "kind": "ask",
                "question": args.get("question"),
                "header": args.get("header"),
                "options": args.get("options"),
                "multiSelect": args.get("multi_select").and_then(|v| v.as_bool()).unwrap_or(false),
                "allowOther": args.get("allow_other").and_then(|v| v.as_bool()).unwrap_or(false)
            }),
            cfg,
            http,
        )
        .await
        .map(format_dialog_result),

        "form" => render_dialog(
            json!({
                "kind": "form",
                "title": args.get("title"),
                "description": args.get("description"),
                "header": args.get("header"),
                "fields": args.get("fields"),
                "actions": args.get("actions"),
                "submitLabel": args.get("submit_label"),
                "cancelLabel": args.get("cancel_label")
            }),
            cfg,
            http,
        )
        .await
        .map(format_dialog_result),

        "aiui_health" => get_json(http, cfg, "/health").await.map(value_to_tool_text),
        "version" => get_json(http, cfg, "/version").await.map(value_to_tool_text),
        "update" => post_empty(http, cfg, "/update")
            .await
            .map(value_to_tool_text),

        _ => {
            return Ok(json!({
                "content": [{"type": "text", "text": format!("unknown tool: {name}")}],
                "isError": true
            }));
        }
    };

    match outcome {
        Ok(v) => Ok(v),
        Err(e) => Ok(json!({
            "content": [{"type": "text", "text": format!("aiui tool error: {e}")}],
            "isError": true
        })),
    }
}

// ---------- dialog/http plumbing ----------

fn load_token(cfg: &AppConfig) -> Result<String, String> {
    std::fs::read_to_string(&cfg.token_path)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("reading token: {e}"))
}

fn base_url(cfg: &AppConfig) -> String {
    format!("http://127.0.0.1:{}", cfg.http_port)
}

async fn render_dialog(
    spec: Value,
    cfg: &AppConfig,
    http: &reqwest::Client,
) -> Result<Value, String> {
    let token = load_token(cfg)?;
    let url = format!("{}/render", base_url(cfg));
    let body = json!({ "spec": spec });
    let resp = http
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST /render: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("render http {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("parse /render: {e}"))
}

async fn get_json(
    http: &reqwest::Client,
    cfg: &AppConfig,
    path: &str,
) -> Result<Value, String> {
    let token = load_token(cfg)?;
    let url = format!("{}{}", base_url(cfg), path);
    let resp = http
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("GET {path}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{path} http {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("parse {path}: {e}"))
}

async fn post_empty(
    http: &reqwest::Client,
    cfg: &AppConfig,
    path: &str,
) -> Result<Value, String> {
    let token = load_token(cfg)?;
    let url = format!("{}{}", base_url(cfg), path);
    let resp = http
        .post(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("POST {path}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{path} http {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("parse {path}: {e}"))
}

// MCP tool-result shape: { content: [...], structuredContent?: ..., isError? }
fn value_to_tool_text(v: Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
        }],
        "structuredContent": v
    })
}

fn format_confirm_result(render: Value) -> Value {
    // /render returns { id, cancelled, result }; for confirm, result is
    // { confirmed: bool } on submit, or null on cancel.
    let cancelled = render
        .get("cancelled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let confirmed = render
        .get("result")
        .and_then(|r| r.get("confirmed"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let payload = json!({ "cancelled": cancelled, "confirmed": confirmed });
    value_to_tool_text(payload)
}

fn format_dialog_result(render: Value) -> Value {
    // Passthrough: just return what the frontend delivered. The agent gets
    // whatever shape the widget produced (values for form, answers for ask).
    let cancelled = render
        .get("cancelled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut payload = render
        .get("result")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("cancelled".into(), json!(cancelled));
    } else {
        payload = json!({ "cancelled": cancelled });
    }
    value_to_tool_text(payload)
}

// ---------- prompts ----------

fn prompts_list() -> Value {
    json!([
        {
            "name": "widgets",
            "description": "Full widget catalog, rules, and patterns for building aiui dialogs. Load at the start of UI-heavy work.",
            "arguments": []
        },
        {
            "name": "update",
            "description": "Check for an aiui update and install it silently, reporting the outcome.",
            "arguments": []
        },
        {
            "name": "version",
            "description": "Report the currently installed aiui version.",
            "arguments": []
        }
    ])
}

fn prompts_get(params: Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let text = match name.as_str() {
        "widgets" => SKILL_MD,
        "update" => UPDATE_PROMPT,
        "version" => VERSION_PROMPT,
        _ => {
            return Err(RpcError {
                code: -32602,
                message: format!("unknown prompt: {name}"),
            });
        }
    };
    Ok(json!({
        "description": format!("aiui:{name}"),
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": text }
        }]
    }))
}
