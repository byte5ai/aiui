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

/// Top-level instruction string returned in the `initialize` MCP response.
/// Loaded by the client (Claude Code / Claude Desktop) as a session-wide
/// hint, *before* the agent decides how to respond to anything. This is
/// what shifts the agent's default away from chat-first replies on the
/// kind of prompts aiui is meant to handle.
const INSTRUCTIONS: &str = "\
aiui is connected — you can render native dialogs on the user's Mac \
instead of asking via chat. Default behaviour for this session:

- Yes/no question (esp. before delete / drop / force-push / deploy) → \
  call `confirm` instead of asking in chat.
- Pick-one-of-N options where context per option matters → call `ask`.
- Multiple related inputs, secret, date, slider, sortable order, \
  table-row triage, image confirm/grid → call `form`.
- Pure information the user only reads → keep it in chat.

Type `/aiui:teach` for the full widget catalog when composing a \
complex form.
";

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

const HEALTH_PROMPT: &str = "\
Run the `aiui_health` tool and report the result in one short sentence:

- If `ready: true`, say \"aiui ready (v{version})\".
- If `ready: false`, point at the most likely cause based on the response \
  body (WebView frozen, dialog backlog, too many children) and suggest the \
  one-step fix (\"open Settings, click Check for updates\" or \"restart aiui\").

Don't dump the raw JSON unless the user asked for it.
";

const TEST_DIALOG_PROMPT: &str = "\
Open a small demo dialog so the user can verify aiui is wired up end to end. \
Call the `confirm` tool with:

  title: \"aiui test dialog\"
  message: \"Click any button — this just verifies the wiring.\"
  header: \"Demo\"
  confirm_label: \"It works\"
  cancel_label: \"Close\"

Report the outcome in one line: \"aiui ok — you clicked '{label}'\" if the \
window opened and returned, or the underlying error if it didn't.
";

const REMOTES_PROMPT: &str = "\
Show the user a quick rundown of their registered aiui remotes — same as \
the Settings window's \"Eingerichtete Remote-Hosts\" section, but in chat. \
Hit the companion's GET /health endpoint via `aiui_health` first to make \
sure aiui is up; if it isn't, just tell the user that and stop. Otherwise \
read the user's `~/.config/aiui/remotes.json` (one host per line / JSON \
list) and present them in a compact table with hostname only. If the file \
is missing or empty, say \"no remotes registered yet — open Settings to \
add one\".
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

    // Track the wall-clock instant of the last incoming line so we can
    // double-check the idle deadline after a sleep. `tokio::time::sleep`
    // is monotonic-clock based and *should* compose correctly across
    // suspend/resume on macOS, but we've seen reports of timer-drift
    // edge cases — verifying with `Instant::now()` on wake is cheap
    // insurance against premature exit. Issue #H-4 in v0.4.10 review.
    let mut last_activity = std::time::Instant::now();

    loop {
        let line = tokio::select! {
            res = reader.next_line() => match res {
                Ok(Some(l)) => {
                    last_activity = std::time::Instant::now();
                    l
                }
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
                // Double-check actual elapsed time before exiting. If the
                // host suspended for a long stretch, the sleep can fire
                // earlier than expected after wake; bail out only if the
                // wall-clock confirms we've actually been idle.
                let elapsed = last_activity.elapsed();
                if elapsed >= STDIN_IDLE_LIMIT {
                    trace(&format!(
                        "mcp-stdio: no input for {:?}, parent likely gone, exiting",
                        elapsed
                    ));
                    return;
                }
                trace(&format!(
                    "mcp-stdio: idle-timer fired but only {:?} elapsed (likely post-suspend); rearming",
                    elapsed
                ));
                continue;
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
            },
            // MCP `instructions` is the only spec-sanctioned way to push a
            // top-level hint into every session at handshake time. We use it
            // to break the LLM's chat-first default — without this nudge, the
            // skill description and tool descriptions are passive triggers
            // that rarely fire on plain "Should I … ?" prompts. Kept short
            // (≤ 500 chars) on purpose; the full widget catalog still lives
            // in `prompts/get widgets`.
            "instructions": INSTRUCTIONS
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
            "description": "Before writing any yes/no question into chat, call this tool instead. Pass `destructive: true` (red button) for delete / drop / force-push / rollback / prod-deploy — never trust loose prior approval for irreversible steps; re-confirm in a dialog. For visual sign-off (\"is this image OK?\", \"keep this generated diagram?\") pass `image: {src, alt?, max_height?}` — `src` accepts data: URLs, http(s) URLs, or absolute / `~/`-rooted local paths (resolved on YOUR host). Returns {cancelled, confirmed}. For 3+ options, use `ask`. For pure information the user only reads, render in chat.",
            "inputSchema": {
                "type": "object",
                "required": ["title"],
                "properties": {
                    "title": { "type": "string", "description": "Decision as a question, ≤ 10 words." },
                    "message": { "type": "string", "description": "One sentence stating the concrete consequence." },
                    "header": { "type": "string", "description": "Short chip above the title (≤ 14 chars)." },
                    "destructive": { "type": "boolean", "default": false, "description": "Red confirm button — for deletions/rollbacks only." },
                    "confirm_label": { "type": "string" },
                    "cancel_label": { "type": "string" },
                    "image": {
                        "type": "object",
                        "description": "Optional image shown between header and title for visual sign-off.",
                        "required": ["src"],
                        "properties": {
                            "src": { "type": "string", "description": "data: URL, http(s):// URL, or absolute / ~/ local path on YOUR host. Same resolution rules as the form `image` field." },
                            "alt": { "type": "string" },
                            "max_height": { "type": "number" }
                        }
                    }
                }
            }
        },
        {
            "name": "ask",
            "description": "Before listing options in chat and waiting for the user to type back which one (deploy strategy, migration path, file to act on …), call this tool instead. Per-option `description` carries the trade-off; `multi_select` and `allow_other` cover the rest. For visual choice (\"which of these images?\") pass `thumbnail: <src>` per option — same resolution rules as anywhere else in aiui (data:, http(s)://, or absolute local path). Returns {cancelled, answers, other?}. For yes/no, use `confirm`. For ≥ 2 related inputs, use `form`.",
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
                                "value": { "type": "string" },
                                "thumbnail": { "type": "string", "description": "Optional image src shown next to the option label. Same resolution rules as the form image field." }
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
            "description": "Whenever the user needs to provide ≥ 2 related inputs, or any single input that doesn't belong in chat (secret, date/datetime/range, bounded number, sortable ranking, multi-select, color pick, table-row triage with column context, image confirm/grid), call this tool instead of typing the questions one by one. Fields: text, password, number, select, checkbox, slider, date, datetime, date_range, color, static_text, markdown, image, mermaid, image_grid, list, table, tree. Group long forms with `tabs: [{label, fields: [...]}]` (one submit, all tabs validated). Footer actions are top-level on the form (`actions: [...]`), NOT inside a tab — they always render at the window's bottom. Action variants: primary (blue), success (green), destructive (red). Returns {cancelled, action?, values}. For yes/no, use `confirm`. For one-of-N pick, use `ask`. Sortable list field shape (most common stumble — always include `value` per item): {\"kind\":\"list\",\"name\":\"rank\",\"label\":\"Sortieren\",\"sortable\":true,\"items\":[{\"label\":\"A\",\"value\":\"a\"},{\"label\":\"B\",\"value\":\"b\"}]}. Image fields (`image`, `image_grid`, list-item `thumbnail`): `src` accepts (1) an absolute or `~/`-rooted local path — aiui's bridge on YOUR host reads it and inlines as `data:`; (2) an `http(s)://` URL — Mac-companion fetches and inlines; (3) a `data:` URL — pass through. Pick the path form when the file is on disk on your host. Relative paths and cross-host paths don't resolve. Never base64-roundtrip through a shell pipeline — build the `data:` URL in your runtime. For schematic visualisations (flowcharts, sequence/state diagrams, gantt, mind-maps) use the `mermaid` field instead of ASCII art: `{\"kind\":\"mermaid\",\"source\":\"graph TD; A --> B; B --> C\"}`. See the aiui skill for the full field catalog.",
            "inputSchema": {
                "type": "object",
                "required": ["title"],
                "properties": {
                    "title": { "type": "string" },
                    "fields": { "type": "array", "items": { "type": "object" }, "description": "Flat field list. Use this OR `tabs`, not both." },
                    "tabs": {
                        "type": "array",
                        "description": "Tab-grouped fields for longer forms. Each tab has its own set of fields. One submit covers all tabs; validation surfaces the first invalid tab automatically.",
                        "items": {
                            "type": "object",
                            "required": ["label", "fields"],
                            "properties": {
                                "label": { "type": "string" },
                                "fields": { "type": "array", "items": { "type": "object" } }
                            }
                        }
                    },
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

/// How long mcp-stdio waits for the aiui HTTP endpoint to become reachable
/// before giving up on a tool call. The dominant case this catches: the
/// user closed the GUI via the window's red X (which exits the app),
/// then immediately triggered a render call — `mcp_attach`'s
/// auto-resurrect (see `lifetime.rs`) IS firing in parallel and brings
/// the GUI back, but Claude's tool call would otherwise race ahead and
/// hit a not-yet-bound port. Eight seconds covers a realistic cold-start
/// (Tauri init + WebView load + HTTP bind) on a normal Mac.
const COLDSTART_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

/// Poll `/ping` until the HTTP server answers, or `COLDSTART_WAIT` elapses.
/// `/ping` is unauthenticated and cheap, returning `pong` in plain text —
/// any 2xx means aiui is bound and serving. Returns `true` once reachable,
/// `false` on timeout. Issue surfaced 2026-04-27 when a fresh Claude
/// session ran the demo prompt right after the user X-closed the GUI.
async fn wait_for_aiui(http: &reqwest::Client, cfg: &AppConfig) -> bool {
    let url = format!("http://127.0.0.1:{}/ping", cfg.http_port);
    let deadline = std::time::Instant::now() + COLDSTART_WAIT;
    loop {
        let probe = http
            .get(&url)
            .timeout(std::time::Duration::from_millis(800))
            .send()
            .await;
        if let Ok(r) = probe {
            if r.status().is_success() {
                return true;
            }
        }
        if std::time::Instant::now() >= deadline {
            trace(&format!(
                "mcp-stdio: aiui /ping not reachable after {:?}, giving up",
                COLDSTART_WAIT
            ));
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// Tool-call response signaling that the local aiui companion isn't
/// reachable. Phrased as user-facing guidance rather than a raw error
/// because Claude tends to relay this verbatim to the user.
fn aiui_unreachable_result() -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": "aiui companion is not reachable on localhost:7777. \
                     If you're on the local machine: open aiui from /Applications. \
                     If you're on a remote dev host: the SSH-reverse-tunnel to your \
                     Mac is down — check that aiui is running there and the tunnel \
                     in aiui Settings shows 'connected'."
        }],
        "isError": true
    })
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

    // Cold-start gate: every tool we expose hits the local HTTP server.
    // Wait for it to become reachable instead of returning a connection-
    // refused error the moment we get one — that masks the auto-resurrect
    // path's startup window cleanly.
    if !wait_for_aiui(http, cfg).await {
        return Ok(aiui_unreachable_result());
    }

    let outcome = match name.as_str() {
        "confirm" => render_dialog(
            json!({
                "kind": "confirm",
                "title": args.get("title"),
                "message": args.get("message"),
                "header": args.get("header"),
                "destructive": args.get("destructive").and_then(|v| v.as_bool()).unwrap_or(false),
                "confirmLabel": args.get("confirm_label"),
                "cancelLabel": args.get("cancel_label"),
                "image": args.get("image")
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
                "tabs": args.get("tabs"),
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
    // Resolve any absolute / `~/`-rooted file paths in `src` /
    // `thumbnail` to `data:` URLs *here* — at the bridge — because
    // this code runs on whichever host the agent is talking to. For
    // local Mac use that's the same host as the GUI server; for
    // SSH-tunneled remotes the agent and this binary live on the
    // remote, where the actual files are. The Mac-side server-resolver
    // (imageresolve::resolve_image_srcs) only knows about HTTPS — it
    // would never see the remote's filesystem.
    let mut spec = spec;
    crate::imageresolve::resolve_local_paths(&mut spec);
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
            "name": "teach",
            "description": "Brief the agent on aiui. Loads the full widget catalog, design rules, and anti-patterns into the session. Run once per project so the agent reaches for the right dialog without further prompting.",
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
        },
        {
            "name": "health",
            "description": "One-line aiui health check: WebView responsive, no dialog backlog, no child-process flood.",
            "arguments": []
        },
        {
            "name": "test-dialog",
            "description": "Pop a tiny demo dialog so the user can verify aiui is wired up end to end.",
            "arguments": []
        },
        {
            "name": "remotes",
            "description": "List the user's registered aiui remotes in chat (same set the Settings window shows).",
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
        "teach" => SKILL_MD,
        "update" => UPDATE_PROMPT,
        "version" => VERSION_PROMPT,
        "health" => HEALTH_PROMPT,
        "test-dialog" => TEST_DIALOG_PROMPT,
        "remotes" => REMOTES_PROMPT,
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
