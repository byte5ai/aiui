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
use tokio::sync::mpsc;

/// How often the companion fires `notifications/progress` while a
/// `confirm`/`ask`/`form` tool call waits on the user. Picked to land
/// well below MCP-client default timeouts (Claude Desktop ≈ 60 s,
/// Claude Code ≈ 120 s) so the notification clearly signals "still
/// alive" before any client-side give-up. v0.4.40.
const PROGRESS_NOTIFY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

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

/// Top-level entry: read JSON-RPC messages from stdin, dispatch to handlers,
/// write responses to stdout. Runs until stdin closes (parent gone).
///
/// v0.4.45 removed the `STDIN_IDLE_LIMIT` self-exit (was 6 h). It was a
/// workaround for the 2026-04-25 stale-mcp-stdio-accumulation incident,
/// but it assumed "no input for 6 h ⇒ parent gone" — which is false for
/// the common case "user simply didn't run an agent overnight". The
/// timer fired every night, the child self-exited, the lifetime grace
/// timer then tore down the GUI 60 s later, and Claude Desktop (which
/// does not auto-respawn a disconnected MCP server) showed "Server
/// disconnected" until the user restarted it. The genuine "parent
/// gone" case is caught cleanly by stdin-EOF below; the stale-child
/// accumulation the timer was meant to prevent is now covered three
/// other ways (sibling-kill, periodic disk_version_if_stale, pre-GUI
/// kill). So: no timer, just block on stdin.
///
/// Outgoing traffic flows through an mpsc channel rather than directly
/// onto stdout, so that a long-running tool call (waiting on the user
/// in `confirm`/`ask`/`form`) can interleave `notifications/progress`
/// onto the wire from a side-task without racing for the stdout lock.
/// v0.4.40.
pub async fn run_stdio(cfg: Arc<AppConfig>) {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("reqwest client");

    // Single writer task drains the channel onto stdout. Capacity 128
    // is comfortable for one-response-at-a-time + ~6 progress
    // notifications per minute per active tool call.
    let (tx, mut rx) = mpsc::channel::<Value>(128);
    let writer_task = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = rx.recv().await {
            if stdout
                .write_all(format!("{msg}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
            let _ = stdout.flush().await;
        }
    });

    trace("mcp-stdio: run_stdio entered");

    loop {
        // Block on stdin. EOF (`Ok(None)`) means the parent closed the
        // pipe — that's the one true "parent gone" signal. No idle
        // timer: an idle-but-alive parent must keep us alive (v0.4.45).
        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => {
                trace("mcp-stdio: stdin closed, exiting");
                break;
            }
            Err(e) => {
                trace(&format!("mcp-stdio: stdin error: {e}, exiting"));
                break;
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

        // Each request gets its own dispatch task so progress
        // notifications can be sent in parallel via the writer
        // channel. Tasks share `cfg` and `http` (both `Clone`-able);
        // the channel is the sync point.
        let cfg_for_task = cfg.clone();
        let http_for_task = http.clone();
        let tx_for_task = tx.clone();
        let method_owned = method.to_string();
        tokio::spawn(async move {
            let response = match dispatch(
                &method_owned,
                params,
                &cfg_for_task,
                &http_for_task,
                &tx_for_task,
            )
            .await
            {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(err) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": err.code, "message": err.message }
                }),
            };
            let _ = tx_for_task.send(response).await;
        });
    }

    // Close the channel so the writer task drains and exits. Without
    // this, exit waits forever on the still-open sender.
    drop(tx);
    let _ = writer_task.await;
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
    tx: &mpsc::Sender<Value>,
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
        "tools/call" => tools_call(params, cfg, http, tx).await,
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
            "description": "Before writing any yes/no question into chat, call this tool instead. Pass `destructive: true` (red button) for delete / drop / force-push / rollback / prod-deploy — never trust loose prior approval for irreversible steps; re-confirm in a dialog. For visual sign-off (\"is this image OK?\", \"keep this generated diagram?\") pass `image: {src, alt?, max_height?}` — `src` accepts data: URLs, http(s) URLs, or absolute / `~/`-rooted local paths (resolved on YOUR host). Returns {cancelled, confirmed}. For 3+ options, use `ask`. For pure information the user only reads, render in chat. **This tool blocks until the user clicks a button. Response can take minutes — do not assume aiui is broken on slow response, the user is just thinking. The companion sends MCP progress notifications every ~10 s while waiting.**",
            "inputSchema": {
                "type": "object",
                "required": ["title"],
                "properties": {
                    "title": { "type": "string", "description": "Decision as a question, ≤ 10 words." },
                    "session": { "type": "string", "description": "Optional short human label for the session this dialog belongs to (project/task name). Shown in the window chrome so the user can tell parallel dialogs apart." },
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
            "description": "Before listing options in chat and waiting for the user to type back which one (deploy strategy, migration path, file to act on …), call this tool instead. Per-option `description` carries the trade-off; `multi_select` and `allow_other` cover the rest. For visual choice (\"which of these images?\") pass `thumbnail: <src>` per option — same resolution rules as anywhere else in aiui (data:, http(s)://, or absolute local path). Returns {cancelled, answers, other?}. For yes/no, use `confirm`. For ≥ 2 related inputs, use `form`. **This tool blocks until the user picks an option or cancels. Response can take minutes — do not assume aiui is broken on slow response. Progress notifications fire every ~10 s while waiting.**",
            "inputSchema": {
                "type": "object",
                "required": ["question", "options"],
                "properties": {
                    "session": { "type": "string", "description": "Optional short human label for the session this dialog belongs to (project/task name). Shown in the window chrome so the user can tell parallel dialogs apart." },
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
            "description": "Whenever the user needs to provide ≥ 2 related inputs, or any single input that doesn't belong in chat (secret, date/datetime/range, bounded number, sortable ranking, multi-select, color pick, table-row triage with column context, image confirm/grid), call this tool instead of typing the questions one by one. Fields: text, password, number, select, checkbox, slider, date, datetime, date_range, color, static_text, markdown, image, mermaid, wireframe, image_grid, list, table, tree. Group long forms with `tabs: [{label, fields: [...]}]` (one submit, all tabs validated). Footer actions are top-level on the form (`actions: [...]`), NOT inside a tab — they always render at the window's bottom. Action variants: primary (blue), success (green), destructive (red). Returns {cancelled, action?, values}. For yes/no, use `confirm`. For one-of-N pick, use `ask`. Sortable list field shape (most common stumble — always include `value` per item): {\"kind\":\"list\",\"name\":\"rank\",\"label\":\"Sortieren\",\"sortable\":true,\"items\":[{\"label\":\"A\",\"value\":\"a\"},{\"label\":\"B\",\"value\":\"b\"}]}. Image fields (`image`, `image_grid`, list-item `thumbnail`): `src` accepts (1) an absolute or `~/`-rooted local path — aiui's bridge on YOUR host reads it and inlines as `data:`; (2) an `http(s)://` URL — Mac-companion fetches and inlines; (3) a `data:` URL — pass through. Pick the path form when the file is on disk on your host. Relative paths and cross-host paths don't resolve. Never base64-roundtrip through a shell pipeline — build the `data:` URL in your runtime. For schematic visualisations (flowcharts, sequence/state diagrams, gantt, mind-maps) use the `mermaid` field instead of ASCII art: `{\"kind\":\"mermaid\",\"source\":\"graph TD; A --> B; B --> C\"}`. For UI-layout mockups (dashboard tiles, hardware-UI panels, login screens, anything with fixed-position boxes-and-labels) use the `wireframe` field — declarative panel grid, NOT ASCII boxes-and-pipes: `{\"kind\":\"wireframe\",\"columns\":3,\"panels\":[{\"title\":\"STATUS\",\"content\":\"Tiefe: 18 m\\nKurs: 270°\",\"col_span\":1},{\"title\":\"EMPFANG\",\"content\":\"14:32 [STARK]…\",\"col_span\":2}]}`. Each panel has optional `title` (uppercase header), `content` (multi-line monospace text, escape `\\n`), `col_span`/`row_span` (default 1), and `tone` (\"default\"/\"muted\"/\"highlight\"). See the aiui skill for the full field catalog. **This tool blocks until the user submits or cancels. Response can take minutes (longer for complex forms) — do not assume aiui is broken on slow response, the user is filling the form. The companion sends MCP progress notifications every ~10 s while waiting.**",
            "inputSchema": {
                "type": "object",
                "required": ["title"],
                "properties": {
                    "session": { "type": "string", "description": "Optional short human label for the session this dialog belongs to (project/task name). Shown in the window chrome so the user can tell parallel dialogs apart." },
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
                    "cancel_label": { "type": "string" },
                    "size": { "type": "string", "enum": ["s", "m", "l"], "description": "Starting window size hint: s (compact), m (roomy), l (large). aiui picks good local defaults and clamps to the screen. The window is always resizable; this only sets the *initial* size, and never opens smaller than the content needs. Use m/l for forms with images, tables, wireframes, or many fields so they don't open cramped." },
                    "width": { "type": "number", "description": "Explicit starting window width in logical px (overrides `size`). Rarely needed — prefer `size`." },
                    "height": { "type": "number", "description": "Explicit starting window height in logical px (overrides `size`). Rarely needed — prefer `size`." }
                }
            }
        },
        {
            "name": "gallery",
            "description": "Batch visual review: show several images and/or videos at once and collect a per-item decision (+ optional comment) in ONE window, instead of calling `confirm` once per asset. Use this for \"review these N generated images\", \"triage this batch of screenshots\", \"approve/revise/skip each of these renders\". Each item needs a stable `value` (the key you get decisions back under) and a `src` (data: URL, http(s):// URL, or absolute / `~/`-rooted local path on YOUR host — same resolution rules as the form `image` field; videos are detected by data:video/ MIME or .mp4/.mov/.m4v/.webm extension and rendered with native controls). Per-item buttons come from `actions` (default Approve / Revise / Skip); set `comment: true` to show a free-text field per item. Returns {cancelled, decisions: {\"<item value>\": {decision, comment?}}} — only items the user touched appear. For a single image sign-off use `confirm` with `image`; for one-of-N choice use `ask` with thumbnails. **Blocks until the user submits or cancels. Response can take minutes — progress notifications fire every ~10 s.**",
            "inputSchema": {
                "type": "object",
                "required": ["items"],
                "properties": {
                    "session": { "type": "string", "description": "Optional short human label for the session this dialog belongs to (project/task name). Shown in the window chrome so the user can tell parallel dialogs apart." },
                    "title": { "type": "string", "description": "What the user is reviewing, e.g. \"Review 6 hero renders\"." },
                    "description": { "type": "string", "description": "One sentence of context shown under the title." },
                    "header": { "type": "string", "description": "Short chip above the title (≤ 14 chars)." },
                    "items": {
                        "type": "array",
                        "description": "The assets to review. Order is preserved.",
                        "items": {
                            "type": "object",
                            "required": ["value"],
                            "properties": {
                                "value": { "type": "string", "description": "Stable id; keys the returned decision. Must be non-empty and unique." },
                                "src": { "type": "string", "description": "Image or video source: data: URL, http(s):// URL, or absolute / ~/ local path on YOUR host." },
                                "alt": { "type": "string" },
                                "label": { "type": "string", "description": "Caption shown under the thumbnail." },
                                "detail": { "type": "string", "description": "Short context line beside/under the label." },
                                "max_height": { "type": "number", "description": "Cap thumbnail height in px." }
                            }
                        }
                    },
                    "actions": {
                        "type": "array",
                        "description": "Per-item decision buttons. Defaults to Approve (green) / Revise / Skip if omitted.",
                        "items": {
                            "type": "object",
                            "required": ["label", "value"],
                            "properties": {
                                "label": { "type": "string" },
                                "value": { "type": "string", "description": "Returned as the item's `decision`." },
                                "primary": { "type": "boolean" },
                                "success": { "type": "boolean" },
                                "destructive": { "type": "boolean" }
                            }
                        }
                    },
                    "comment": { "type": "boolean", "default": false, "description": "Show a free-text comment field per item." },
                    "columns": { "type": "number", "description": "Grid columns. Omit for responsive auto-fill." },
                    "submit_label": { "type": "string" },
                    "cancel_label": { "type": "string" },
                    "size": { "type": "string", "enum": ["s", "m", "l"], "description": "Starting window size hint: s / m / l. Default auto-sizes to the item count; pass l for a large batch or tall thumbnails so the grid opens roomy. Always resizable; never opens smaller than the content needs." },
                    "width": { "type": "number", "description": "Explicit starting window width in logical px (overrides `size`)." },
                    "height": { "type": "number", "description": "Explicit starting window height in logical px (overrides `size`)." }
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
/// GUI is mid-cold-start (auto-resurrect after the user closed it, or a
/// fresh `open --auto` from `mcp_attach`) and Claude's tool call would
/// otherwise race ahead and hit a not-yet-bound port.
///
/// v0.4.45 (Bug #4): raised 8 s → 30 s. A full cold start (Tauri init +
/// WebView load + HTTP bind + lifetime-socket + tunnels) can exceed 8 s
/// on a busy Mac, and the 2026-05-26 incident showed a tool call dying
/// at the 8 s mark while the GUI was still coming up. 30 s comfortably
/// covers a worst-case cold start and is still well under any sane
/// MCP-client tool timeout, so a genuinely-down aiui still fails fast
/// enough to surface the diagnostic message.
const COLDSTART_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

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

/// Tool-call response signaling that the local aiui companion didn't
/// answer `/ping` within `COLDSTART_WAIT`. Differentiates the realistic
/// causes so the calling agent can choose between "retry once" and
/// "tell the user something useful". v0.4.36 rewrite: the previous
/// generic "not reachable on localhost:7777" was relayed verbatim by
/// Claude even when the actual cause was contention (parallel session,
/// stale dialog window, multiple aiui calls in one assistant turn) —
/// none of which the user can fix by re-opening aiui.
fn aiui_unreachable_result() -> Value {
    let local = crate::lifetime::is_interactive_session();
    let context_line = if local {
        "You are running on the user's local Mac (no SSH session detected)."
    } else {
        "You are running on a remote dev host. aiui reaches the user's Mac \
         via an SSH-reverse-tunnel on port 7777."
    };
    let text = format!(
        "aiui companion did not answer /ping on localhost:7777 within {} seconds.\n\
         \n\
         {context_line}\n\
         \n\
         Likely causes (in order of frequency):\n\
         1. **Multiple aiui calls in one assistant turn.** The previous call \
            held the dialog window; the second one raced ahead before the \
            companion freed the slot. Retry the failing call once after a \
            short wait — usually succeeds.\n\
         2. **Stale dialog window from an earlier session.** A previous \
            agent's dialog timed out or was orphaned and is still pinning \
            the companion. Tell the user: \"please close any leftover aiui \
            dialog windows on your Mac and try again.\"\n\
         3. **A parallel Claude session is using aiui right now.** Two \
            agents on the same Mac share one companion; only one dialog at \
            a time. Either retry shortly or tell the user the other session \
            is currently holding the dialog.\n\
         4. **aiui is genuinely not running** (cold-start path). If you are \
            on the user's Mac, ask them to open aiui from /Applications. If \
            on a remote host, the SSH-reverse-tunnel may be down — point \
            them to aiui Settings → Connections.\n\
         \n\
         Do not relay this entire message to the user verbatim — pick the \
         likely cause and phrase it plainly.",
        COLDSTART_WAIT.as_secs()
    );
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true
    })
}

async fn tools_call(
    params: Value,
    cfg: &Arc<AppConfig>,
    http: &reqwest::Client,
    tx: &mpsc::Sender<Value>,
) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    // MCP progress notifications. The client opts in by passing a token
    // in `params._meta.progressToken` (string or integer). While the
    // tool blocks on the user, we send a `notifications/progress`
    // every PROGRESS_NOTIFY_INTERVAL with the same token so the
    // client knows we're alive — keeps Claude Desktop and Claude
    // Code from concluding "tool hung" mid-dialog. v0.4.40.
    let progress_token = params
        .get("_meta")
        .and_then(|m| m.get("progressToken"))
        .cloned();
    let progress_handle = if let Some(token) = progress_token {
        let tx_clone = tx.clone();
        Some(tokio::spawn(async move {
            let mut elapsed_secs: u64 = 0;
            loop {
                tokio::time::sleep(PROGRESS_NOTIFY_INTERVAL).await;
                elapsed_secs += PROGRESS_NOTIFY_INTERVAL.as_secs();
                let notif = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {
                        "progressToken": token,
                        "progress": elapsed_secs,
                        "message": format!("aiui still waiting on user ({elapsed_secs}s)"),
                    }
                });
                if tx_clone.send(notif).await.is_err() {
                    break;
                }
            }
        }))
    } else {
        None
    };

    // Cold-start gate: every tool we expose hits the local HTTP server.
    // Wait for it to become reachable instead of returning a connection-
    // refused error the moment we get one — that masks the auto-resurrect
    // path's startup window cleanly.
    if !wait_for_aiui(http, cfg).await {
        if let Some(h) = progress_handle {
            h.abort();
        }
        return Ok(aiui_unreachable_result());
    }

    let outcome = match name.as_str() {
        "confirm" => dispatch_render(
            render_dialog(
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
                args.get("session").and_then(|v| v.as_str()).map(String::from),
                cfg,
                http,
            )
            .await,
            format_confirm_result,
        ),

        "ask" => dispatch_render(
            render_dialog(
                json!({
                    "kind": "ask",
                    "question": args.get("question"),
                    "header": args.get("header"),
                    "options": args.get("options"),
                    "multiSelect": args.get("multi_select").and_then(|v| v.as_bool()).unwrap_or(false),
                    "allowOther": args.get("allow_other").and_then(|v| v.as_bool()).unwrap_or(false)
                }),
                args.get("session").and_then(|v| v.as_str()).map(String::from),
                cfg,
                http,
            )
            .await,
            format_dialog_result,
        ),

        "form" => dispatch_render(
            render_dialog(
                json!({
                    "kind": "form",
                    "title": args.get("title"),
                    "description": args.get("description"),
                    "header": args.get("header"),
                    "fields": args.get("fields"),
                    "tabs": args.get("tabs"),
                    "actions": args.get("actions"),
                    "submitLabel": args.get("submit_label"),
                    "cancelLabel": args.get("cancel_label"),
                    "size": args.get("size"),
                    "width": args.get("width"),
                    "height": args.get("height")
                }),
                args.get("session").and_then(|v| v.as_str()).map(String::from),
                cfg,
                http,
            )
            .await,
            format_dialog_result,
        ),

        "gallery" => dispatch_render(
            render_dialog(
                json!({
                    "kind": "gallery",
                    "title": args.get("title"),
                    "description": args.get("description"),
                    "header": args.get("header"),
                    "items": args.get("items"),
                    "actions": args.get("actions"),
                    "comment": args.get("comment").and_then(|v| v.as_bool()).unwrap_or(false),
                    "columns": args.get("columns"),
                    "submitLabel": args.get("submit_label"),
                    "cancelLabel": args.get("cancel_label"),
                    "size": args.get("size"),
                    "width": args.get("width"),
                    "height": args.get("height")
                }),
                args.get("session").and_then(|v| v.as_str()).map(String::from),
                cfg,
                http,
            )
            .await,
            format_dialog_result,
        ),

        "aiui_health" => get_json(http, cfg, "/health").await.map(value_to_tool_text),
        "version" => get_json(http, cfg, "/version").await.map(value_to_tool_text),
        "update" => post_empty(http, cfg, "/update")
            .await
            .map(value_to_tool_text),

        _ => {
            if let Some(h) = progress_handle {
                h.abort();
            }
            return Ok(json!({
                "content": [{"type": "text", "text": format!("unknown tool: {name}")}],
                "isError": true
            }));
        }
    };

    if let Some(h) = progress_handle {
        h.abort();
    }

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

/// Push a local video file to the companion's `POST /media` cache and return
/// the playback URL it hands back. Reads the file on *this* host (local Mac,
/// or the remote for an SSH-tunneled session) and uploads the bytes over the
/// same :7777 channel the render goes through — so it works identically
/// local and remote without any Mac→remote access. Errors (file unreadable,
/// 413, old companion without `/media` → 404) bubble up; the caller treats
/// them as non-fatal and leaves the original path in place.
async fn upload_media(
    http: &reqwest::Client,
    cfg: &AppConfig,
    token: &str,
    path: &str,
) -> Result<String, String> {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(h) => h.join(rest),
            None => std::path::PathBuf::from(path),
        }
    } else {
        std::path::PathBuf::from(path)
    };
    let bytes = tokio::fs::read(&expanded)
        .await
        .map_err(|e| format!("read {}: {e}", expanded.display()))?;
    let ext = crate::imageresolve::video_ext(path);
    let url = format!("{}/media?ext={}", base_url(cfg), ext);
    let resp = http
        .post(&url)
        .bearer_auth(token)
        .header("content-type", "application/octet-stream")
        .body(bytes)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("POST /media: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("/media http {}", resp.status()));
    }
    let body = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("parse /media: {e}"))?;
    body.get("url")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "/media response missing url".to_string())
}

/// Per-call dialog rendering can fail in two structurally different
/// ways. v0.4.36 splits them so the tool dispatcher can convert
/// `Busy` into a structured tool result (with retry-vs-tell-user
/// guidance) instead of bubbling it up as a generic transport error
/// the way "render http 409" used to.
enum RenderError {
    /// The companion answered 409 — another dialog is already in
    /// flight. Carries the diagnostic counts the HTTP layer reported.
    Busy {
        pending_count: u64,
        oldest_age_secs: u64,
    },
    /// Transport, parse, status-other-than-409, or token failures.
    /// Surfaced as a generic "aiui tool error" to the agent, since
    /// these are conditions the user actually has to act on.
    Transport(String),
}

async fn render_dialog(
    spec: Value,
    session: Option<String>,
    cfg: &AppConfig,
    http: &reqwest::Client,
) -> Result<Value, RenderError> {
    let token = load_token(cfg).map_err(RenderError::Transport)?;
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
    // Video (2026-05-31): local video files are too big to inline as `data:`
    // (10 MB cap, base64 bloat), so push them to the companion's /media cache
    // and swap the path for the returned loopback playback URL. Done BEFORE
    // `resolve_local_paths` so the image inliner never tries to base64 a
    // video. Upload failures are non-fatal — the path is simply left as-is
    // (the WebView shows a broken player rather than the call blowing up).
    let videos = crate::imageresolve::collect_local_video_paths(&spec);
    if !videos.is_empty() {
        let mut map = std::collections::HashMap::new();
        for path in videos {
            match upload_media(http, cfg, &token, &path).await {
                Ok(media_url) => {
                    map.insert(path, media_url);
                }
                Err(e) => trace(&format!("render_dialog: media upload failed for {path}: {e}")),
            }
        }
        crate::imageresolve::replace_srcs(&mut spec, &map);
    }
    crate::imageresolve::resolve_local_paths(&mut spec);
    // Step 4 (I8): forward the optional caller `session` label. This is the
    // local bridge, so there is no `session_origin` (the companion treats an
    // absent origin as local).
    let body = json!({ "spec": spec, "session": session });
    // Async render (Step 3): POST opts in via `x-aiui-async`; the companion
    // registers + surfaces the dialog and returns immediately with
    // `{id, ttl_secs}` (202). We then poll `GET /render/{id}` in bounded
    // windows until the terminal result. No single connection is held for the
    // user's think-time, so a tunnel/GUI blip can cost at most one poll
    // window — never a multi-minute ReadError. The POST itself only covers
    // registration + the ack handshake, so a short timeout suffices.
    //
    // Backward-compatible: an older companion ignores the unknown header and
    // answers synchronously (200 with the terminal `{cancelled, …}` shape) —
    // detected after the status checks below and used directly, no polling.
    let resp = http
        .post(&url)
        .bearer_auth(&token)
        .header("x-aiui-async", "1")
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| RenderError::Transport(format!("POST /render: {e}")))?;
    if resp.status() == reqwest::StatusCode::CONFLICT {
        // Body shape from http::render: { error, pending_count, oldest_age_secs }.
        let body = resp.json::<Value>().await.unwrap_or(Value::Null);
        return Err(RenderError::Busy {
            pending_count: body
                .get("pending_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(1),
            oldest_age_secs: body
                .get("oldest_age_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        });
    }
    if resp.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        // Invalid spec — http::render rejected it *before* showing any
        // window (v0.4.46, Bug B+). Body shape: { error, detail, hint }.
        // Surface detail+hint as the tool error so the agent understands
        // exactly what's malformed and can fix the spec and retry — no
        // confusing fallback window, no terse status code.
        let body = resp.json::<Value>().await.unwrap_or(Value::Null);
        let detail = body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("invalid dialog spec");
        let hint = body.get("hint").and_then(|v| v.as_str()).unwrap_or("");
        let msg = if hint.is_empty() {
            format!("aiui rejected the dialog spec (invalid_spec): {detail}")
        } else {
            format!("aiui rejected the dialog spec (invalid_spec): {detail} — {hint}")
        };
        return Err(RenderError::Transport(msg));
    }
    if !resp.status().is_success() {
        return Err(RenderError::Transport(format!(
            "render http {}",
            resp.status()
        )));
    }
    let accepted = resp.status() == reqwest::StatusCode::ACCEPTED;
    let first = resp
        .json::<Value>()
        .await
        .map_err(|e| RenderError::Transport(format!("parse /render: {e}")))?;
    if !accepted {
        // Synchronous companion (old): `first` is already the terminal result.
        return Ok(first);
    }
    // Async companion: poll `GET /render/{id}` until terminal. Each GET is
    // bounded (40 s > the server's ~25 s poll window) so the server always
    // answers `{pending:true}` before we time out, and we re-poll. The loop
    // ends on the terminal result, a 404 (id expired / never registered), or
    // the server-side TTL turning into a terminal `cancelled` result.
    let id = match first.get("id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return Err(RenderError::Transport(
                "async /render: 202 response missing `id`".into(),
            ))
        }
    };
    let poll_url = format!("{}/render/{}", base_url(cfg), id);
    loop {
        let pr = http
            .get(&poll_url)
            .bearer_auth(&token)
            .timeout(std::time::Duration::from_secs(40))
            .send()
            .await
            .map_err(|e| RenderError::Transport(format!("GET /render/{id}: {e}")))?;
        if pr.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RenderError::Transport(format!(
                "aiui lost track of render {id} (expired or never registered)"
            )));
        }
        if !pr.status().is_success() {
            return Err(RenderError::Transport(format!(
                "render poll http {}",
                pr.status()
            )));
        }
        let pv = pr
            .json::<Value>()
            .await
            .map_err(|e| RenderError::Transport(format!("parse /render/{id}: {e}")))?;
        if pv.get("pending").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        return Ok(pv);
    }
}

/// Tool-call response signaling that the companion is alive but
/// already serving another dialog (multi-call-per-turn, second Claude
/// session, stale window). Phrased as agent-facing guidance, parallel
/// to `aiui_unreachable_result`.
fn aiui_busy_result(pending_count: u64, oldest_age_secs: u64) -> Value {
    let text = format!(
        "aiui companion is busy serving another dialog right now \
         (pending={pending_count}, oldest_age={oldest_age_secs}s).\n\
         \n\
         The companion intentionally serves only one dialog at a time. \
         The other dialog is one of:\n\
         1. **Your previous aiui call in this same assistant turn.** Tools \
            in one turn run sequentially and the prior dialog hasn't been \
            answered yet. Wait until the user answers, then issue the next \
            call. Do not retry rapidly.\n\
         2. **A stale dialog window from an earlier session** that the \
            user never answered. Tell the user: \"please answer or close \
            the leftover aiui dialog on your Mac, then I'll retry.\" The \
            companion will sweep it automatically after 5 minutes.\n\
         3. **A parallel Claude session is currently using aiui.** Either \
            wait briefly and retry, or tell the user the other session \
            holds the dialog right now.\n\
         \n\
         Do not relay this entire message to the user verbatim — pick the \
         likely cause and phrase it plainly."
    );
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true
    })
}

/// Dispatch a `render_dialog` outcome into a tool-call result. `Busy`
/// becomes a successful tool call with `isError: true` and diagnostic
/// guidance; `Transport` becomes an `Err` that `tools_call` then
/// renders as the generic "aiui tool error: …" path.
fn dispatch_render(
    res: Result<Value, RenderError>,
    formatter: fn(Value) -> Value,
) -> Result<Value, String> {
    match res {
        Ok(v) => Ok(formatter(v)),
        Err(RenderError::Busy {
            pending_count,
            oldest_age_secs,
        }) => Ok(aiui_busy_result(pending_count, oldest_age_secs)),
        Err(RenderError::Transport(s)) => Err(s),
    }
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
