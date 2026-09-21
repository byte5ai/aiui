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
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// How often the companion fires `notifications/progress` while a
/// `confirm`/`ask`/`form` tool call waits on the user. Picked to land
/// well below MCP-client default timeouts (Claude Desktop ≈ 60 s,
/// Claude Code ≈ 120 s) so the notification clearly signals "still
/// alive" before any client-side give-up. v0.4.40.
const PROGRESS_NOTIFY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Budget for a best-effort `DELETE /render/{id}` when a request is cancelled
/// or the host quits (#193). Short on purpose: retracting a dialog must never
/// be what keeps this process alive.
const CANCEL_RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Upper bound on draining the stdout writer at EOF (#193). Belt-and-braces —
/// a stuck writer must never pin the process, which is exactly the stale
/// `--mcp-stdio` child class this codebase has fought repeatedly.
const WRITER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Where an in-flight tool call publishes the companion-side render id, as soon
/// as the async `202` is parsed. `notifications/cancelled` and stdin EOF read it
/// to issue `DELETE /render/{id}`, so a dialog is never left on the user's Mac
/// waiting for an agent that is gone. `None` until a dialog is registered — a
/// tool call that never rendered has nothing to retract.
type RenderSink = Arc<Mutex<Option<String>>>;

fn new_render_sink() -> RenderSink {
    Arc::new(Mutex::new(None))
}

/// One dispatched JSON-RPC request, keyed by `id.to_string()`.
///
/// The key is the *stringified* `Value`: `serde_json::Value` is not `Hash`, and
/// `to_string()` canonicalises both the string and the integer id forms the
/// spec allows, so the registration and the later `notifications/cancelled`
/// lookup agree.
struct InFlight {
    task: tokio::task::JoinHandle<()>,
    render_id: RenderSink,
}

type InFlightMap = Arc<Mutex<HashMap<String, InFlight>>>;

/// Aborts the wrapped task when dropped.
///
/// The progress loop is spawned inside `tools_call` and was only aborted on the
/// explicit success/error paths — so aborting a dispatch task left its progress
/// loop running, still holding a clone of the writer channel's sender, and the
/// EOF drain then waited forever on it (#193). Owned by `tools_call`, it dies
/// with its parent however the parent ends.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl AbortOnDrop {
    fn abort(&self) {
        self.0.abort();
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Best-effort `DELETE /render/{id}` — ask the companion to retract a dialog
/// this bridge no longer has a caller for. Every failure is swallowed: an older
/// companion has no such route (404/405), and a companion that is already gone
/// has no dialog to retract either.
async fn cancel_render(http: reqwest::Client, cfg: Arc<AppConfig>, render_id: String) {
    let Ok(token) = load_token(&cfg) else {
        return;
    };
    let url = format!("{}/render/{}", base_url(&cfg), render_id);
    match http
        .delete(&url)
        .bearer_auth(token)
        .timeout(CANCEL_RENDER_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => trace(&format!(
            "mcp-stdio: DELETE /render/{render_id} → {}",
            r.status()
        )),
        Err(e) => trace(&format!("mcp-stdio: DELETE /render/{render_id} failed: {e}")),
    }
}

/// Handle a `notifications/cancelled`: abort the dispatch task for
/// `params.requestId` and hand back its published render id, if any, so the
/// caller can retract the dialog.
///
/// An unknown `requestId` is a no-op — a cancel that races the response is
/// normal and must not disturb the other in-flight entries. Split out of the
/// run loop so the abort semantics are unit-testable.
fn handle_cancelled(in_flight: &mut HashMap<String, InFlight>, params: &Value) -> Option<String> {
    let key = params.get("requestId")?.to_string();
    let entry = in_flight.remove(&key)?;
    entry.task.abort();
    let render_id = entry.render_id.lock().unwrap().clone();
    trace(&format!(
        "mcp-stdio: cancelled request {key}, render_id={render_id:?}"
    ));
    render_id
}

const SKILL_MD: &str = include_str!("../../../docs/skill.md");

/// Top-level instruction string returned in the `initialize` MCP response.
/// Loaded by the client (Claude Code / Claude Desktop) as a session-wide
/// hint, *before* the agent decides how to respond to anything. This is
/// what shifts the agent's default away from chat-first replies on the
/// kind of prompts aiui is meant to handle.
const INSTRUCTIONS: &str = "\
aiui is connected — you can render native dialogs on the user's machine \
instead of asking via chat. Default behaviour for this session:

- Yes/no question (esp. before delete / drop / force-push / deploy) → \
  call `confirm` instead of asking in chat.
- Pick-one-of-N options where context per option matters → call `ask`.
- Multiple related inputs, secret, date, slider, sortable order, \
  table-row triage, image confirm/grid → call `form`.
- User wants to hand you a file from their machine (`/aiui:upload`, \
  \"take this file\", \"upload …\") → call `upload` with the target \
  directory on your host; don't ask them to `scp` it.
- Async-completion signal the user doesn't need to answer (tests green, \
  deploy done, merge conflict) → call `notify` — it returns immediately, \
  no dialog, no reply expected.
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
- If `updated: false` and `note` mentions a dialog in flight, report that \
  aiui {available} is ready but was not installed because a dialog is \
  still open on the user's machine — installing would close it and \
  discard what they typed. Ask them to finish it, then run /aiui:update \
  again. Do not retry on your own.
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
- If `ready: false`, read `reason` and `hint` from the response body and \
  relay the `hint` — it already names the cause and the one-step fix, with \
  the live numbers filled in. Don't guess a cause the body doesn't state. \
  Only if `hint` is absent, fall back to \"restart aiui\".
- `ready: false` with `reason: \"dialog_registry_full\"` or \
  `\"too_many_children\"` is degraded, not down: say so, because dialogs \
  still render.

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

const UPLOAD_PROMPT: &str = "\
Call the `upload` tool to let me hand you a file from my machine. \
Use my current working directory as the target unless I say otherwise.
";

const REMOTES_PROMPT: &str = "\
Show the user a quick rundown of their registered aiui remotes — same as \
the Settings window's \"Eingerichtete Remote-Hosts\" section, but in chat. \
Hit the companion's GET /health endpoint via `aiui_health` first to make \
sure aiui is up; if it isn't, just tell the user that and stop. Otherwise \
read the user's `remotes.json` from aiui's config directory — \
`~/.config/aiui/` on macOS and Linux, `%APPDATA%\\aiui\\` on Windows (one \
host per line / JSON list) — and present them in a compact table with \
hostname only. If the file \
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
    run_stdio_io(
        cfg,
        tokio::io::stdin(),
        tokio::io::stdout(),
        Arc::new(Mutex::new(HashMap::new())),
    )
    .await
}

/// `run_stdio` over injectable transports and an injectable in-flight map, so
/// the cancellation and EOF paths can be driven in tests without touching the
/// process's real stdio.
async fn run_stdio_io<R, W>(cfg: Arc<AppConfig>, input: R, output: W, in_flight: InFlightMap)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(input).lines();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("reqwest client");

    // Single writer task drains the channel onto stdout. Capacity 128
    // is comfortable for one-response-at-a-time + ~6 progress
    // notifications per minute per active tool call.
    let (tx, mut rx) = mpsc::channel::<Value>(128);
    let writer_task = tokio::spawn(async move {
        let mut stdout = output;
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

        // Notifications (no id). `notifications/cancelled` is the MCP spec's
        // only way to abort an in-flight request — Esc in Claude Code — and
        // dropping it here (#193) meant the bridge kept polling, kept emitting
        // progress for a token the client had already forgotten, and the dialog
        // stayed on the user's Mac until answered or the 2 h TTL fired.
        // Everything else is still silently dropped per JSON-RPC spec.
        let Some(id) = id_opt else {
            if method == "notifications/cancelled" {
                let render_id = handle_cancelled(&mut in_flight.lock().unwrap(), &params);
                if let Some(render_id) = render_id {
                    let http_for_cancel = http.clone();
                    let cfg_for_cancel = cfg.clone();
                    tokio::spawn(cancel_render(http_for_cancel, cfg_for_cancel, render_id));
                }
            }
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
        let key = id.to_string();
        let render_sink = new_render_sink();
        let sink_for_task = render_sink.clone();
        let in_flight_for_task = in_flight.clone();
        let key_for_task = key.clone();
        // Spawn and register under one lock: the task removes its own entry
        // through this same mutex, so it cannot finish and remove *before* the
        // registration lands and leave a stale handle behind.
        let mut registry = in_flight.lock().unwrap();
        let task = tokio::spawn(async move {
            let response = match dispatch(
                &method_owned,
                params,
                &cfg_for_task,
                &http_for_task,
                &tx_for_task,
                &sink_for_task,
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
            in_flight_for_task.lock().unwrap().remove(&key_for_task);
        });
        registry.insert(
            key,
            InFlight {
                task,
                render_id: render_sink,
            },
        );
        drop(registry);
    }

    // EOF — the parent is gone. Abort every in-flight dispatch task first: each
    // holds a clone of `tx`, and `rx.recv()` only ends when *all* senders are
    // dropped, so without this the drain below waited for as long as any tool
    // call was outstanding — which for a dialog is up to the 2 h TTL, leaving a
    // stale child alive with `lifetime::mcp_attach` still attached (#193).
    // Retract their dialogs on the way out so quitting the host doesn't leave a
    // window on the user's Mac.
    let entries: Vec<InFlight> = in_flight.lock().unwrap().drain().map(|(_, v)| v).collect();
    let mut cancels = Vec::new();
    for entry in entries {
        entry.task.abort();
        let render_id = entry.render_id.lock().unwrap().clone();
        if let Some(render_id) = render_id {
            cancels.push(tokio::spawn(cancel_render(
                http.clone(),
                cfg.clone(),
                render_id,
            )));
        }
    }
    for handle in cancels {
        let _ = tokio::time::timeout(CANCEL_RENDER_TIMEOUT, handle).await;
    }

    // Close the channel so the writer task drains and exits. Without
    // this, exit waits forever on the still-open sender. Bounded either way:
    // a stuck writer must never be what keeps this process in RAM.
    drop(tx);
    let _ = tokio::time::timeout(WRITER_DRAIN_TIMEOUT, writer_task).await;
}

#[derive(Debug)]
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
    render_sink: &RenderSink,
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
        "tools/call" => tools_call(params, cfg, http, tx, render_sink).await,
        "prompts/list" => Ok(json!({ "prompts": prompts_list() })),
        "prompts/get" => prompts_get(params),
        // MCP requires a prompt empty result for `ping`; a sender may treat a
        // failed ping as a stale connection and terminate the session — which
        // surfaces as the "Server disconnected" symptom. Unlike
        // `server/discover`, this must NEVER fall through to -32601.
        "ping" => Ok(json!({})),
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
                    "confirm_label": { "type": "string", "description": "Overrides the affirmative button label. Defaults to the companion's localized affirmative label — resolved from the user's locale, so don't name it in chat unless you set it yourself." },
                    "cancel_label": { "type": "string", "description": "Overrides the negative button label. Defaults to the companion's localized negative label — resolved from the user's locale, so don't name it in chat unless you set it yourself." },
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
            "description": "Whenever the user needs to provide ≥ 2 related inputs, or any single input that doesn't belong in chat (secret, date/datetime/range, bounded number, sortable ranking, multi-select, color pick, table-row triage with column context, image confirm/grid, audio playback), call this tool instead of typing the questions one by one. Fields: text, password, secret, number, select, checkbox, slider, date, datetime, date_range, color, static_text, markdown, image, annotated_image, audio, mermaid, wireframe, image_grid, list, table, tree. **Audio field (#25):** `{\"kind\":\"audio\",\"src\":\"...\",\"label\":\"...\"}` — read-only native `<audio controls>` player, for \"listen to this TTS sample / voice memo / generated sound clip before deciding\". `src` accepts a `data:audio/...` URL, an `http(s)://` URL, or an absolute/`~/`-rooted local path (mp3/m4a/wav/aac/ogg/flac) — local audio is pushed through the same size-unbounded `/media` cache as gallery video, never the 10 MB `data:` inliner, so large clips work too. **File-write / secret capture (#135):** any input field may carry an optional `target` to write the entered value to a file ON THE HOST THE AGENT RUNS ON when the user submits (the affirmative button IS the per-write approval; the user sees the path first): `{\"kind\":\"secret\",\"name\":\"pat\",\"label\":\"GitHub PAT\",\"target\":{\"mode\":\"create\",\"path\":\"~/.github_tokens/byte5ai\",\"perm\":\"0600\",\"overwrite\":true}}`. `mode`: `create` (write raw value; needs `overwrite:true` to clobber) or `substitute` (replace a `placeholder` that occurs exactly once in an existing file — for YAML/TOML/INI/etc; choose a DISTINCTIVE sentinel that can't collide with real file content, e.g. `__AIUI_SECRET_GITHUB_PAT__`, not a common word — if it occurs 0 or >1 times the write is refused with an error, never misapplied to the wrong spot). A `secret`-kind field is **write-only**: its value is NEVER returned to you (result carries only `{written, target, bytes}`); use it precisely so a credential the user types never enters this conversation. A `secret` therefore REQUIRES a `target` — without one the render is rejected with `invalid_spec` rather than handing you the plaintext; if you want the value back, that field is a `password`, not a `secret`. Non-secret fields with a `target` are written AND returned. **Only an affirmative action commits the write:** the submit button or a plain named action. An action carrying `skip_validation:true` (your Cancel / Save-draft escape hatch) writes nothing and returns `{written:false, error}` per field — set `writes_targets:true` on it if it really must write. A blank field also writes nothing (`refusing to write an empty value`), in both modes, so a skipped optional field never truncates the user's file and `substitute` never erases its own sentinel. The destination is always the agent's own host: the aiui module already running there (the native app locally, the bridge on a remote SSH session) performs the write as a LOCAL file operation, so `create` and `substitute` both work identically local and remote — and you cannot target a foreign host. Errors come back as `{written:false, error}`. Group long forms with `tabs: [{label, fields: [...]}]` (one submit, all tabs validated). Footer actions are top-level on the form (`actions: [...]`), NOT inside a tab — they always render at the window's bottom. Action variants: primary (blue), success (green), destructive (red). Returns {cancelled, action?, values}. For yes/no, use `confirm`. For one-of-N pick, use `ask`. Sortable list field shape (most common stumble — always include `value` per item): {\"kind\":\"list\",\"name\":\"rank\",\"label\":\"Sortieren\",\"sortable\":true,\"items\":[{\"label\":\"A\",\"value\":\"a\"},{\"label\":\"B\",\"value\":\"b\"}]}. Image fields (`image`, `image_grid`, list-item `thumbnail`): `src` accepts (1) an absolute or `~/`-rooted local path — aiui's bridge on YOUR host reads it and inlines as `data:`; (2) an `http(s)://` URL — the companion fetches and inlines; (3) a `data:` URL — pass through. Pick the path form when the file is on disk on your host. Relative paths and cross-host paths don't resolve. Never base64-roundtrip through a shell pipeline — build the `data:` URL in your runtime. To have the user MARK a spot on an image (logo placement, crop hint, bug location) use `annotated_image`: `{\"kind\":\"annotated_image\",\"name\":\"spot\",\"src\":\"~/shot.png\",\"mode\":\"point\"}` — `mode` is `point` (click one marker, default), `region` (drag a rectangle), or `both` (user flips a Point/Region tool). `src` follows the same resolution rules as `image`. Returns normalized 0..1 coords under the field name: `{\"point\":{\"x\",\"y\"}|null,\"region\":{\"x\",\"y\",\"w\",\"h\"}|null,\"natural\":{\"width\",\"height\"}|null}` — multiply by `natural` for pixels. For schematic visualisations (flowcharts, sequence/state diagrams, gantt, mind-maps) use the `mermaid` field instead of ASCII art: `{\"kind\":\"mermaid\",\"source\":\"graph TD; A --> B; B --> C\"}`. For UI-layout mockups (dashboard tiles, hardware-UI panels, login screens, anything with fixed-position boxes-and-labels) use the `wireframe` field — declarative panel grid, NOT ASCII boxes-and-pipes: `{\"kind\":\"wireframe\",\"columns\":3,\"panels\":[{\"title\":\"STATUS\",\"content\":\"Tiefe: 18 m\nKurs: 270°\",\"col_span\":1},{\"title\":\"EMPFANG\",\"content\":\"14:32 [STARK]…\",\"col_span\":2}]}`. Each panel has optional `title` (uppercase header), `content` (multi-line monospace text, escape `\n`), `col_span`/`row_span` (default 1), and `tone` (\"default\"/\"muted\"/\"highlight\"). See the aiui skill for the full field catalog. **This tool blocks until the user submits or cancels. Response can take minutes (longer for complex forms) — do not assume aiui is broken on slow response, the user is filling the form. The companion sends MCP progress notifications every ~10 s while waiting.**",
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
            "name": "upload",
            "description": "Pull a file FROM the user's machine INTO this agent session. Calling this opens a native file picker on the user's machine; the file they choose is streamed back over aiui's channel and written to `target_dir` on YOUR host (the machine you run on — the remote for an SSH session). This is the counterpart to the user having to `scp` a file over: reach for it whenever the user says \"take this file\", \"upload …\", \"here's the file/screenshot/PDF\", or triggers `/aiui:upload`. **`target_dir` is optional and you should almost always pass it:** set it to the directory the file belongs in given the conversation — usually your current working directory or the active project dir. Do NOT ask the user where to put it or which file to pick; just call the tool and let them choose the file in the native dialog. If you have no context at all, omit `target_dir` (defaults to your process's cwd) or ask in one short sentence. The filename comes from the user's selection — the file lands at `target_dir/<filename>`, a deterministic path, no temp/staging dir. **Existing files are never overwritten:** if `target_dir/<filename>` already exists the call returns an error rather than clobbering — pick a different `target_dir` or move the old file first. Returns `{status: \"ok\", path, filename, bytes}` on success, or `{status: \"error\", error}` on any failure — user cancelled the picker, file unreadable, file too large (512 MB cap), or target directory missing/not writable. Report the result briefly; on `ok` mention the path the file landed at. **This tool blocks until the user picks a file or dismisses the picker. Response can take a while — do not assume aiui is broken; the user is choosing a file. Progress notifications fire every ~10 s while waiting.**",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target_dir": { "type": "string", "description": "Absolute or `~/`-rooted directory ON YOUR HOST where the picked file is written as `<target_dir>/<filename>`. Optional; defaults to your process's current working directory. Relative paths are rejected (no stable cwd contract). The directory must already exist and be writable." },
                    "session": { "type": "string", "description": "Optional short human label for the session this upload belongs to (project/task name), shown in the file picker's title bar so the user can tell which agent asked for a file." }
                }
            }
        },
        {
            "name": "compare",
            "description": "Side-by-side A/B (or A/B/C) compare: render 2+ variants as full-content panes next to each other and let the user click ONE to pick. Use this instead of `ask`+thumbnail (which only shows a small icon per option, not the full content) or `gallery` (per-item batch review — approve/revise/skip each, not a single pick). Fits \"which draft is better\", \"which image edit\", \"before vs. after\", \"which of these three headlines\". Each variant needs a stable `value` (the key returned as `selected`) and at least one of `content` (markdown text — drafts, diffs, code) or `src` (image/video, same resolution rules as elsewhere: data: URL, http(s):// URL, or absolute / `~/`-rooted local path on YOUR host; videos render with native controls). A variant may carry both — e.g. an image plus a caption. Set `sync_scroll: true` when comparing long text so scrolling one pane scrolls all of them together. If `max_height` is set on any variant, it caps every pane's height (equal-height panes read as \"side by side\"; independent per-pane heights don't). Returns {cancelled, selected} — `selected` is the `value` of the picked variant (only set when the user actually submits; Cancel/Escape leaves it absent). For a plain yes/no use `confirm`; for choosing among many small thumbnails use `ask`+thumbnail or `image_grid`; for per-item batch verdicts use `gallery`. **Blocks until the user picks and submits or cancels. Response can take minutes — progress notifications fire every ~10 s.**",
            "inputSchema": {
                "type": "object",
                "required": ["variants"],
                "properties": {
                    "session": { "type": "string", "description": "Optional short human label for the session this dialog belongs to (project/task name). Shown in the window chrome so the user can tell parallel dialogs apart." },
                    "title": { "type": "string", "description": "What's being compared, e.g. \"Which intro paragraph?\"." },
                    "description": { "type": "string", "description": "One sentence of context shown under the title." },
                    "header": { "type": "string", "description": "Short chip above the title (≤ 14 chars)." },
                    "variants": {
                        "type": "array",
                        "minItems": 2,
                        "description": "The options to compare, rendered as equal-width panes in order. Needs at least 2 (A/B) — 3 for A/B/C.",
                        "items": {
                            "type": "object",
                            "required": ["value"],
                            "properties": {
                                "value": { "type": "string", "description": "Stable id; returned as `selected` when this variant is picked. Must be non-empty." },
                                "label": { "type": "string", "description": "Pane header. Defaults to A / B / C / … by position." },
                                "content": { "type": "string", "description": "Markdown text for a text variant — draft copy, a before/after diff, code." },
                                "src": { "type": "string", "description": "Image or video source: data: URL, http(s):// URL, or absolute / ~/ local path on YOUR host." },
                                "alt": { "type": "string" },
                                "detail": { "type": "string", "description": "Short caption under the pane — source, score, timestamp." },
                                "max_height": { "type": "number", "description": "Cap this pane's height in px. If set on ANY variant, it applies to ALL panes so they stay equal-height." }
                            }
                        }
                    },
                    "sync_scroll": { "type": "boolean", "default": false, "description": "Lock scroll position across all panes — useful when comparing long text side by side." },
                    "columns": { "type": "number", "description": "Override the number of columns. Defaults to variants.length, capped at 4." },
                    "submit_label": { "type": "string" },
                    "cancel_label": { "type": "string" },
                    "size": { "type": "string", "enum": ["s", "m", "l"], "description": "Starting window size hint: s / m / l. Default auto-sizes to variant count and content. Always resizable; never opens smaller than the content needs." },
                    "width": { "type": "number", "description": "Explicit starting window width in logical px (overrides `size`)." },
                    "height": { "type": "number", "description": "Explicit starting window height in logical px (overrides `size`)." }
                }
            }
        },
        {
            "name": "notify",
            "description": "Fire a native OS notification and return immediately — use this for an async-completion signal to a user who isn't watching this session (\"tests green\", \"deploy finished\", \"merge conflicts, need you\"). Unlike confirm/ask/form/gallery, this tool does NOT wait for the user: it hands the notification to the OS and returns {ok: true} right away, with no dialog, no window, no response to parse. Use it instead of a chat message when the point is exactly that the user doesn't have to be looking at this session to notice. For anything that needs an answer (yes/no, a choice, input), use confirm/ask/form — notify has no way to carry a reply back. `title` is required and short (≤ ~40 chars, notification banners truncate); `body` carries the detail. `subtitle` is optional extra context (folded into the body on platforms without a distinct subtitle slot). `sound` is an optional OS sound name (e.g. \"default\"); omit for a silent notification.",
            "inputSchema": {
                "type": "object",
                "required": ["title", "body"],
                "properties": {
                    "title": { "type": "string", "description": "Short headline, ≤ ~40 chars — notification banners truncate longer text." },
                    "body": { "type": "string", "description": "The detail — what finished, what needs attention." },
                    "subtitle": { "type": "string", "description": "Optional extra context line." },
                    "sound": { "type": "string", "description": "Optional OS notification sound name (e.g. \"default\"). Omit for silent." }
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
            "description": "Check for an aiui update, download-and-install if one is available, then relaunch silently. Responds BEFORE the relaunch so the caller receives {updated, current, available, note}. Next agent call hits the new version. Returns {updated: false, note: \"dialog in flight — update deferred\"} instead of installing while a dialog is waiting on the user.",
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

/// How long the `upload` tool waits on `POST /upload` before giving up. Unlike
/// a dialog render — which the async poll loop keeps off a single held
/// connection — the picker + byte transfer runs on one request, and the user
/// may browse their filesystem for a while before choosing. A generous ceiling
/// (well above any think-time a file picker realistically takes) keeps the call
/// alive; the MCP progress notifications reassure the client meanwhile. #146.
const UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

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

/// How to tell the user to launch the companion by hand, per platform
/// (#204). Resolved at *compile* time, not from the runtime environment:
/// this binary is the companion, so the build target IS the user's OS.
/// `lifetime::is_interactive_session()` answers a different question —
/// local vs. SSH — and says nothing about which OS we are on; conflating
/// the two is what made the cold-start path tell Windows users to open
/// `/Applications`.
#[cfg(target_os = "macos")]
const OPEN_HINT: &str = "ask them to open aiui from /Applications";
#[cfg(target_os = "windows")]
const OPEN_HINT: &str = "ask them to open aiui from the Start menu";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const OPEN_HINT: &str = "ask them to start the aiui companion";

/// Tool-call response signaling that the local aiui companion didn't
/// answer `/ping` within `COLDSTART_WAIT`. Differentiates the realistic
/// causes so the calling agent can choose between "retry once" and
/// "tell the user something useful".
///
/// #202: this listed dialog-slot contention (a parallel session, a stale
/// window, two calls in one turn) as causes 1-3 and the real ones last. None
/// of them can produce *this* error: `/ping` is unauthenticated and answers a
/// static "pong" regardless of dialog state, and single-occupancy is gone
/// anyway (see `dialog::DIALOG_HARD_CAP`). The agent was told to pick one
/// cause, so it sent users hunting for dialog windows that do not exist.
/// Only the two causes that can actually silence `/ping` remain.
fn aiui_unreachable_result() -> Value {
    let local = crate::lifetime::is_interactive_session();
    let context_line = if local {
        "You are running on the user's own machine (no SSH session detected)."
    } else {
        "You are running on a remote dev host. aiui reaches the user's machine \
         via an SSH-reverse-tunnel on port 7777."
    };
    let text = format!(
        "aiui companion did not answer /ping on localhost:7777 within {} seconds.\n\
         \n\
         {context_line}\n\
         \n\
         Likely causes (in order of frequency):\n\
         1. **aiui is not running.** /ping is unauthenticated and answers \
            instantly whenever the companion is up, so no answer means no \
            companion. If you are on the user's own machine, {open_hint} — \
            the auto-resurrect path may have been suppressed.\n\
         2. **The SSH reverse-tunnel is down.** If you are on a remote host, \
            port 7777 is forwarded from the user's machine; a dropped tunnel \
            looks exactly like this. Point the user to aiui Settings → \
            Connections to re-establish it.\n\
         \n\
         Do not relay this entire message to the user verbatim — pick the \
         likely cause and phrase it plainly.",
        COLDSTART_WAIT.as_secs(),
        open_hint = OPEN_HINT
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
    render_sink: &RenderSink,
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
    // Wrapped in `AbortOnDrop` (#193): if *this* call's task is aborted — the
    // client cancelled the request, or the host quit — the progress loop must
    // die with it. It holds a clone of the writer channel's sender, and a
    // surviving clone is exactly what used to keep the process alive past EOF.
    let progress_handle = if let Some(token) = progress_token {
        let tx_clone = tx.clone();
        Some(AbortOnDrop(tokio::spawn(async move {
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
        })))
    } else {
        None
    };

    // Cold-start gate: every tool we expose hits the local HTTP server.
    // Wait for it to become reachable instead of returning a connection-
    // refused error the moment we get one — that masks the auto-resurrect
    // path's startup window cleanly.
    if !wait_for_aiui(http, cfg).await {
        if let Some(h) = &progress_handle {
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
                render_sink,
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
                render_sink,
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
                render_sink,
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
                render_sink,
            )
            .await,
            format_dialog_result,
        ),

        "upload" => Ok(do_upload(&args, cfg, http).await),

        "compare" => dispatch_render(
            render_dialog(
                json!({
                    "kind": "compare",
                    "title": args.get("title"),
                    "description": args.get("description"),
                    "header": args.get("header"),
                    "variants": args.get("variants"),
                    "syncScroll": args.get("sync_scroll").and_then(|v| v.as_bool()).unwrap_or(false),
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
                render_sink,
            )
            .await,
            format_dialog_result,
        ),

        "notify" => post_json(
            http,
            cfg,
            "/notify",
            compact_object(&args, &["title", "body", "subtitle", "sound"]),
        )
        .await
        .map(value_to_tool_text),

        // Health is the one endpoint whose non-2xx body must survive: a 503
        // carries the `reason`/`hint` the agent is supposed to relay (#179).
        "aiui_health" => get_json_allow_status(http, cfg, "/health")
            .await
            .map(value_to_tool_text),
        "version" => get_json(http, cfg, "/version").await.map(value_to_tool_text),
        "update" => post_empty(http, cfg, "/update")
            .await
            .map(value_to_tool_text),

        _ => {
            if let Some(h) = &progress_handle {
                h.abort();
            }
            return Ok(json!({
                "content": [{"type": "text", "text": format!("unknown tool: {name}")}],
                "isError": true
            }));
        }
    };

    if let Some(h) = &progress_handle {
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

/// Push a local video or audio file to the companion's `POST /media` cache
/// and return the playback URL it hands back. Reads the file on *this* host
/// (local Mac, or the remote for an SSH-tunneled session) and uploads the
/// bytes over the same :7777 channel the render goes through — so it works
/// identically local and remote without any Mac→remote access. Errors (file
/// unreadable, 413, old companion without `/media` → 404) bubble up; the
/// caller treats them as non-fatal and leaves the original path in place.
///
/// `ext` names the cached file's extension (and thus the served
/// Content-Type) — pass [`crate::imageresolve::video_ext`] or
/// [`crate::imageresolve::audio_ext`] depending on which collector matched
/// `path`.
async fn upload_media(
    http: &reqwest::Client,
    cfg: &AppConfig,
    token: &str,
    path: &str,
    ext: &str,
) -> Result<String, String> {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(h) => h.join(rest),
            None => std::path::PathBuf::from(path),
        }
    } else {
        std::path::PathBuf::from(path)
    };
    // Stat first, read second (#194). `tokio::fs::read` on a 3–4 GB screen
    // recording — an entirely ordinary thing to put in a `gallery` item —
    // materialised the whole file in a `Vec<u8>` before anything checked it
    // against the companion's 512 MB ceiling, and an allocation failure there
    // is an OOM kill of the `aiui --mcp-stdio` child, which the MCP host
    // reports as the thoroughly unhelpful "Server disconnected". Same shape
    // as `imageresolve::read_path_as_data_url`.
    let len = tokio::fs::metadata(&expanded)
        .await
        .map_err(|e| format!("read {}: {e}", expanded.display()))?
        .len();
    if len > crate::media::MEDIA_FILE_CAP {
        return Err(format!(
            "{} is {len} bytes, over the {} byte media cap",
            expanded.display(),
            crate::media::MEDIA_FILE_CAP
        ));
    }
    let bytes = tokio::fs::read(&expanded)
        .await
        .map_err(|e| format!("read {}: {e}", expanded.display()))?;
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

/// Percent-decode a filename the companion sent in the `x-aiui-filename`
/// header (RFC-3986, produced by `http::pct_encode_filename`). Returns the raw
/// bytes; invalid `%` sequences are passed through literally rather than
/// erroring, so a mangled header degrades to a slightly-odd name, never a lost
/// upload.
fn pct_decode_bytes(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Reduce a filename to a safe base name: strip any directory components (the
/// companion sends a base name already, but a hostile/odd selection must never
/// escape `target_dir`), reject `.`/`..`/empty. Returns `None` if nothing safe
/// remains.
fn safe_base_name(raw: &str) -> Option<String> {
    let base = std::path::Path::new(raw)
        .file_name()
        .and_then(|n| n.to_str())?
        .trim();
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }
    Some(base.to_string())
}

/// Expand a `~/`-rooted or absolute directory path. Relative paths return
/// `None` — there is no stable cwd contract to resolve them against, so we
/// treat "relative" as a caller error rather than guessing (the cwd default is
/// applied by the caller *before* this, only when `target_dir` is absent).
fn expand_dir(raw: &str) -> Option<std::path::PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    if raw == "~" {
        return dirs::home_dir();
    }
    let p = std::path::PathBuf::from(raw);
    if p.is_absolute() {
        Some(p)
    } else {
        None
    }
}

fn upload_error(msg: impl Into<String>) -> Value {
    value_to_tool_text(json!({ "status": "error", "error": msg.into() }))
}

/// Implements the `upload` tool (#146): ask the companion to open a native file
/// picker on the Mac, receive the picked file's bytes over the :7777 channel,
/// and write them to `target_dir/<filename>` on THIS host. Returns the MCP
/// tool-result shape wrapping `{status, path, filename, bytes}` (ok) or
/// `{status, error}` (any failure, including a user-cancelled picker).
async fn do_upload(args: &Value, cfg: &AppConfig, http: &reqwest::Client) -> Value {
    // Resolve the destination directory up front so a bad `target_dir` fails
    // before we even open the picker (nothing more annoying than picking a file
    // only to be told the target was invalid).
    let target_dir = match args.get("target_dir").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => match expand_dir(s.trim()) {
            Some(p) => p,
            None => {
                return upload_error(format!(
                    "target_dir must be an absolute or ~/-rooted path, got '{s}'"
                ))
            }
        },
        // No target_dir → default to this process's cwd.
        _ => match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => return upload_error(format!("no target_dir given and cwd unavailable: {e}")),
        },
    };
    if !target_dir.is_dir() {
        return upload_error(format!(
            "target directory does not exist: {}",
            target_dir.display()
        ));
    }

    let token = match load_token(cfg) {
        Ok(t) => t,
        Err(e) => return upload_error(e),
    };
    let url = format!("{}/upload", base_url(cfg));
    // #194: the optional `{session}` body titles the picker, so a user facing
    // several agents can tell which one is asking. Additive — an older
    // companion has no body extractor and ignores it.
    let session = args
        .get("session")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let resp = match http
        .post(&url)
        .bearer_auth(&token)
        .json(&json!({ "session": session }))
        .timeout(UPLOAD_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return upload_error(format!("POST /upload: {e}")),
    };

    match resp.status() {
        reqwest::StatusCode::NO_CONTENT => {
            return upload_error("upload cancelled — no file was selected");
        }
        reqwest::StatusCode::CONFLICT => {
            // #194: the companion serialises the native picker. Say so in
            // words the agent can act on, instead of a bare status code.
            return upload_error(
                "another upload is already waiting for the user — one file picker \
                 at a time. Wait for that one to be answered, then retry.",
            );
        }
        reqwest::StatusCode::GATEWAY_TIMEOUT => {
            // #194: the companion's own bound (600 s) fires before this
            // bridge's 900 s, so this is the diagnosis that wins the race.
            return upload_error(
                "the file picker was never answered — it timed out on the user's \
                 machine. Ask the user whether the picker appeared, then retry.",
            );
        }
        reqwest::StatusCode::PAYLOAD_TOO_LARGE => {
            let detail = resp.text().await.unwrap_or_default();
            return upload_error(format!("selected file too large: {detail}"));
        }
        s if !s.is_success() => {
            let detail = resp.text().await.unwrap_or_default();
            return upload_error(format!("companion /upload failed ({s}): {detail}"));
        }
        _ => {}
    }

    // Filename travels in a header (percent-encoded); the body is the raw bytes.
    let filename_raw = resp
        .headers()
        .get("x-aiui-filename")
        .and_then(|v| v.to_str().ok())
        .map(|s| String::from_utf8_lossy(&pct_decode_bytes(s)).into_owned())
        .unwrap_or_default();
    let filename = match safe_base_name(&filename_raw) {
        Some(f) => f,
        None => "upload.bin".to_string(),
    };

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return upload_error(format!("reading uploaded bytes: {e}")),
    };

    let dest = target_dir.join(&filename);
    // Never clobber: a deterministic path is the point, but silently
    // overwriting the user's existing file is not. `write_new` links the
    // file into place atomically, so a file created between here and the
    // write cannot be lost — no check-then-write race (codex review P2).
    match crate::fsutil::write_new(&dest, &bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return upload_error(format!(
                "target already exists, not overwriting: {}",
                dest.display()
            ));
        }
        Err(e) => return upload_error(format!("writing {}: {e}", dest.display())),
    }

    value_to_tool_text(json!({
        "status": "ok",
        "path": dest.display().to_string(),
        "filename": filename,
        "bytes": bytes.len(),
    }))
}

/// Per-call dialog rendering failure. Surfaced as a generic
/// "aiui tool error" to the agent, since these are conditions the user
/// actually has to act on.
///
/// v0.4.36 also carried a `Busy` variant for the companion's 409
/// single-occupancy rejection. That occupancy model is gone (Step 4, I8 —
/// see `dialog::DIALOG_HARD_CAP`): N dialogs may be in flight at once and
/// `http.rs` never answers CONFLICT. The bundled bridge ships in the same
/// binary as the companion it talks to, so it can never meet a 409-era
/// companion either — the variant and its `aiui_busy_result` guidance were
/// removed in #202 rather than left looking like live behaviour.
enum RenderError {
    Transport(String),
}

/// Number of *consecutive* failed polls tolerated before `render_dialog`
/// gives up on an in-flight dialog (#202). Five, one second apart, covers
/// roughly three minutes of outage once the 40 s per-GET timeout is counted
/// in — comfortably more than an SSH reverse-tunnel re-establish or a WebView
/// restart during an in-app update. The counter resets on every successful
/// poll, so a flaky link never accumulates its way to a false give-up.
const POLL_MAX_CONSECUTIVE_FAILURES: u32 = 5;

/// Backoff between two failed polls of the same render id.
const POLL_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(1);

/// Fallback when a 202 body carries no `ttl_secs` — mirrors the companion's
/// `DIALOG_TTL` (2 h).
const DEFAULT_POLL_TTL_SECS: u64 = 7200;

/// Retry budget for the async-render poll loop (#202).
///
/// The async-render design exists so that a connection failure cannot cost
/// the user's think-time: the dialog stays on screen for the whole server-side
/// TTL, so a transport error on one poll is a blip, not an answer. This bounds
/// how long the bridge keeps re-polling the same id — by consecutive failures
/// *and* by the TTL the companion advertised, so we never poll an id that is
/// certainly gone.
struct PollBudget {
    consecutive: u32,
    deadline: std::time::Instant,
}

impl PollBudget {
    fn new(ttl_secs: u64) -> Self {
        PollBudget {
            consecutive: 0,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(ttl_secs),
        }
    }

    /// A poll came back: the link is healthy again, so forget past failures.
    fn on_success(&mut self) {
        self.consecutive = 0;
    }

    /// Count a failed poll. `true` → sleep and re-poll the same id.
    fn may_retry(&mut self) -> bool {
        self.consecutive += 1;
        self.consecutive < POLL_MAX_CONSECUTIVE_FAILURES
            && std::time::Instant::now() < self.deadline
    }

    fn consecutive(&self) -> u32 {
        self.consecutive
    }
}

async fn render_dialog(
    spec: Value,
    session: Option<String>,
    cfg: &AppConfig,
    http: &reqwest::Client,
    render_sink: &RenderSink,
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
    //
    // #194: a failure here used to reach the trace log and nowhere else. The
    // user got a broken player and the agent got no signal at all, so it
    // cheerfully asked about a clip that was never shown. Collected and
    // returned as `media_warnings` instead — still non-fatal, just no longer
    // silent.
    let mut media_warnings: Vec<String> = Vec::new();
    let videos = crate::imageresolve::collect_local_video_paths(&spec);
    if !videos.is_empty() {
        let mut map = std::collections::HashMap::new();
        for path in videos {
            let ext = crate::imageresolve::video_ext(&path);
            match upload_media(http, cfg, &token, &path, &ext).await {
                Ok(media_url) => {
                    map.insert(path, media_url);
                }
                Err(e) => {
                    trace(&format!("render_dialog: media upload failed for {path}: {e}"));
                    media_warnings.push(format!("video not shown — {path}: {e}"));
                }
            }
        }
        crate::imageresolve::replace_srcs(&mut spec, &map);
    }
    // Audio (#25): same reasoning as video — route local audio through the
    // /media cache rather than the 10 MB-capped `data:` inliner, so a form's
    // `audio` field works uniformly regardless of clip size or whether the
    // agent runs locally or on a remote SSH host.
    let audios = crate::imageresolve::collect_local_audio_paths(&spec);
    if !audios.is_empty() {
        let mut map = std::collections::HashMap::new();
        for path in audios {
            let ext = crate::imageresolve::audio_ext(&path);
            match upload_media(http, cfg, &token, &path, &ext).await {
                Ok(media_url) => {
                    map.insert(path, media_url);
                }
                Err(e) => {
                    trace(&format!("render_dialog: media upload failed for {path}: {e}"));
                    media_warnings.push(format!("audio not played — {path}: {e}"));
                }
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
    if resp.status() == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        // #178: the spec exceeded the companion's ceiling — practically
        // always inlined images. Body shape: { error, detail, hint }, same as
        // the 422 above. Without this arm the agent saw only "render http
        // 413", which names neither the image nor a way out.
        let body = resp.json::<Value>().await.unwrap_or(Value::Null);
        let detail = body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("the dialog spec is too large");
        let hint = body.get("hint").and_then(|v| v.as_str()).unwrap_or("");
        let msg = if hint.is_empty() {
            format!("aiui rejected the dialog spec (spec_too_large): {detail}")
        } else {
            format!("aiui rejected the dialog spec (spec_too_large): {detail} — {hint}")
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
        return Ok(attach_media_warnings(first, media_warnings));
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
    // Publish the id the moment it exists (#193). From here a
    // `notifications/cancelled` or stdin EOF can retract the dialog with
    // `DELETE /render/{id}` instead of leaving it on the user's Mac.
    *render_sink.lock().unwrap() = Some(id.clone());
    // #202: the 202 body advertises how long the id stays valid. Both bridges
    // used to read only `id` and throw this away; it is the wall-clock ceiling
    // for the retry budget below.
    let ttl_secs = first
        .get("ttl_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_POLL_TTL_SECS);
    let poll_url = format!("{}/render/{}", base_url(cfg), id);
    let mut budget = PollBudget::new(ttl_secs);
    loop {
        // #202: a transport error here must NOT be terminal. The dialog is
        // already on the user's screen and stays there for the full server-side
        // TTL, so aborting on the first blip abandons an answered window and
        // makes the agent's retry open a second one. Re-poll the SAME id
        // instead — never re-POST /render.
        let pr = match http
            .get(&poll_url)
            .bearer_auth(&token)
            .timeout(std::time::Duration::from_secs(40))
            .send()
            .await
        {
            Ok(pr) => pr,
            Err(e) => {
                if budget.may_retry() {
                    trace(&format!(
                        "render_dialog: poll {id} failed ({e}), retry {}/{}",
                        budget.consecutive(),
                        POLL_MAX_CONSECUTIVE_FAILURES
                    ));
                    tokio::time::sleep(POLL_RETRY_BACKOFF).await;
                    continue;
                }
                return Err(RenderError::Transport(format!(
                    "GET /render/{id}: {e} (gave up after {} consecutive poll \
                     failures — the dialog may still be open on the user's \
                     machine)",
                    budget.consecutive()
                )));
            }
        };
        budget.on_success();
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
        return Ok(attach_media_warnings(pv, media_warnings));
    }
}

/// Fold the render's non-fatal media failures into the render value so the
/// result formatters can hand them to the agent (#194). A no-op when nothing
/// failed — the key only appears when there is something to say.
fn attach_media_warnings(mut render: Value, warnings: Vec<String>) -> Value {
    if warnings.is_empty() {
        return render;
    }
    if let Some(obj) = render.as_object_mut() {
        obj.insert("media_warnings".into(), json!(warnings));
    }
    render
}

/// Copy `media_warnings` from the render value onto the tool payload. Shared
/// by both formatters so `confirm` (which builds its own narrow payload)
/// reports a dropped clip the same way `form`/`gallery` do.
fn carry_media_warnings(render: &Value, payload: &mut Value) {
    let Some(warnings) = render.get("media_warnings") else {
        return;
    };
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("media_warnings".into(), warnings.clone());
    }
}

/// Dispatch a `render_dialog` outcome into a tool-call result: `Transport`
/// becomes an `Err` that `tools_call` then renders as the generic
/// "aiui tool error: …" path.
fn dispatch_render(
    res: Result<Value, RenderError>,
    formatter: fn(Value) -> Value,
) -> Result<Value, String> {
    match res {
        Ok(v) => Ok(formatter(v)),
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

/// Like [`get_json`], but returns the parsed body on *any* status instead of
/// collapsing a non-2xx into a bare status line.
///
/// Only `/health` uses this, deliberately: its body *is* the diagnosis
/// (`reason`, `hint`, `pending`, `oldest_age_secs`, `lifecycle_phase`), and
/// throwing it away to report `"/health http 503 Service Unavailable"` is the
/// bug #179 fixes — `aiui_health` promises to tell a cold companion apart from
/// a rogue process holding the port. `/render`, `/version` and `/update` keep
/// the strict [`get_json`], where a non-2xx genuinely is a failed call.
async fn get_json_allow_status(
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
    let status = resp.status();
    match resp.json::<Value>().await {
        Ok(v) => Ok(v),
        // No JSON to relay — fall back to the status line, which is all we
        // have and still beats an empty error.
        Err(e) if !status.is_success() => {
            Err(format!("{path} http {status} (unparseable body: {e})"))
        }
        Err(e) => Err(format!("parse {path}: {e}")),
    }
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

/// Build a JSON object from the named keys of `args`, skipping keys that are
/// absent or null, so an omitted optional is never posted as an explicit
/// `null` (#203).
///
/// `#[serde(default)]` on the receiving struct fires only for an *absent*
/// key, never for a `null` — so `notify(title="…")` with no `body` used to
/// send `"body": null` and die in axum's `Json` extractor with a 422 whose
/// body is a plain-text serde dump, never reaching the companion's own
/// structured `invalid_request` path.
fn compact_object(args: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        if let Some(v) = args.get(*key).filter(|v| !v.is_null()) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    Value::Object(out)
}

/// POST with a JSON body, returning the parsed response. Backs `notify`
/// (#17) — unlike `render_dialog`, there is no async-poll dance here: the
/// companion's `/notify` handler is itself fire-and-forget and answers
/// synchronously the moment the OS accepts the notification. A non-2xx
/// status surfaces the response body (if any) so a 422 `invalid_request`
/// detail from the companion reaches the agent instead of a bare status
/// code.
async fn post_json(
    http: &reqwest::Client,
    cfg: &AppConfig,
    path: &str,
    body: Value,
) -> Result<Value, String> {
    let token = load_token(cfg)?;
    let url = format!("{}{}", base_url(cfg), path);
    let resp = http
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {path}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = error_detail(&resp.text().await.unwrap_or_default());
        return Err(if detail.is_empty() {
            format!("{path} http {status}")
        } else {
            format!("{path} http {status}: {detail}")
        });
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("parse {path}: {e}"))
}

/// Pull the companion's own `detail` out of a structured error body, falling
/// back to the raw text (#203). The companion answers a bad `/notify` with
/// `{"error":"invalid_request","detail":"title must not be empty"}`; string-
/// concatenating that whole object handed the agent JSON punctuation instead
/// of the sentence written for it. Mirrors the Python bridge's handling.
fn error_detail(raw: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(raw).ok();
    let detail = parsed
        .as_ref()
        .and_then(|v| v.get("detail"))
        .and_then(|d| d.as_str());
    detail.unwrap_or(raw).to_string()
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
    let mut payload = json!({ "cancelled": cancelled, "confirmed": confirmed });
    carry_media_warnings(&render, &mut payload);
    // #202: `format_dialog_result` forwards the companion's cancellation
    // `reason` (ttl_expired / evicted / channel_dropped / host_exiting) but
    // `confirm` — the tool that gates destructive actions — did not. An agent
    // that reports "you declined the migration" after a 2 h TTL expiry is
    // reporting a decision the user never made.
    if cancelled {
        if let Some(reason) = render.get("reason").and_then(|v| v.as_str()) {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("reason".into(), json!(reason));
            }
        }
    }
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
    // #180: forward WHY a dialog was cancelled. The companion sets
    // `host_exiting`, `ttl_expired`, `evicted` and `channel_dropped`;
    // dropping them left the agent unable to tell "the user declined" from
    // "the companion was shutting down". Forwarded generically, so a reason
    // added later needs no bridge change. I6 — the Python bridge does the
    // same.
    if cancelled {
        if let Some(reason) = render.get("reason").and_then(|v| v.as_str()) {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("reason".into(), json!(reason));
            }
        }
    }
    // #194: a clip the bridge could not push to the media cache was shown as
    // a broken player with no word to the agent. Non-fatal, but no longer
    // silent.
    carry_media_warnings(&render, &mut payload);
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
            "description": "Check for an aiui update and install it, reporting the outcome. Deferred while a dialog is open on the user's machine.",
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
        },
        {
            "name": "upload",
            "description": "Hand a file from your machine to the agent session — opens a native file picker and writes the chosen file to the agent host.",
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
        "upload" => UPLOAD_PROMPT,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Pull the JSON payload back out of the MCP tool-result envelope.
    fn tool_payload(v: Value) -> Value {
        let text = v["content"][0]["text"].as_str().expect("text content");
        serde_json::from_str(text).expect("payload is JSON")
    }

    #[test]
    fn cancel_reason_reaches_the_agent() {
        // #180: the companion sets host_exiting / ttl_expired / evicted /
        // channel_dropped, but the bridge flattened every one into a bare
        // {"cancelled": true} — indistinguishable from the user pressing
        // Escape, so an agent retried the wrong thing. `future_reason` stands
        // in for one added later: forwarding is generic on purpose.
        for reason in [
            "host_exiting",
            "ttl_expired",
            "evicted",
            "channel_dropped",
            "future_reason",
        ] {
            let render = json!({
                "id": "d1", "cancelled": true, "result": null, "reason": reason
            });
            let out = tool_payload(format_dialog_result(render));
            assert_eq!(out["cancelled"], json!(true));
            assert_eq!(out["reason"], json!(reason), "reason {reason} must survive");
        }
    }

    #[test]
    fn a_plain_user_cancel_carries_no_reason() {
        // The user pressing Escape has no reason attached, and inventing one
        // would be worse than omitting it.
        let render = json!({"id": "d1", "cancelled": true, "result": null});
        let out = tool_payload(format_dialog_result(render));
        assert_eq!(out["cancelled"], json!(true));
        assert!(out.get("reason").is_none(), "no reason invented: {out}");
    }

    #[test]
    fn a_submitted_dialog_keeps_its_values_and_gains_no_reason() {
        // The happy path must not regress, and a stale reason on a successful
        // submit would be actively misleading.
        let render = json!({
            "id": "d1",
            "cancelled": false,
            "result": {"values": {"name": "Ada"}},
            "reason": "host_exiting"
        });
        let out = tool_payload(format_dialog_result(render));
        assert_eq!(out["cancelled"], json!(false));
        assert_eq!(out["values"]["name"], json!("Ada"));
        assert!(out.get("reason").is_none(), "not a cancel: {out}");
    }

    #[test]
    fn a_dropped_clip_is_reported_to_the_agent() {
        // #194: `upload_media` failures used to reach the trace log only —
        // the user saw a broken player and the agent believed the clip was
        // shown. Both formatters must carry the warning through.
        let render = json!({
            "id": "d1",
            "cancelled": false,
            "result": {"values": {"note": "ok"}},
            "media_warnings": ["video not shown — ~/clip.mp4: too large"]
        });
        let out = tool_payload(format_dialog_result(render.clone()));
        assert_eq!(out["media_warnings"][0], json!("video not shown — ~/clip.mp4: too large"));
        assert_eq!(out["values"]["note"], json!("ok"), "the render result is unchanged");

        let confirm = json!({
            "id": "d2",
            "cancelled": false,
            "result": {"confirmed": true},
            "media_warnings": ["audio not played — ~/vm.mp3: read failed"]
        });
        let out = tool_payload(format_confirm_result(confirm));
        assert_eq!(out["confirmed"], json!(true));
        assert_eq!(out["media_warnings"][0], json!("audio not played — ~/vm.mp3: read failed"));
    }

    #[test]
    fn a_clean_render_carries_no_media_warnings_key() {
        // An always-present empty array trains agents to ignore the key.
        let render = json!({"id": "d1", "cancelled": false, "result": {}});
        let out = tool_payload(format_dialog_result(render));
        assert!(out.get("media_warnings").is_none(), "no warnings invented: {out}");
        assert_eq!(
            attach_media_warnings(json!({"id": "d1"}), vec![]),
            json!({"id": "d1"}),
            "nothing to say → nothing added"
        );
    }

    #[tokio::test]
    async fn upload_media_rejects_oversize_before_reading() {
        // #194: the old code read the file into a Vec<u8> first and only then
        // discovered the companion's 512 MB ceiling — on a 3–4 GB screen
        // recording that is an OOM kill of the mcp-stdio child, surfaced to
        // the user as "Server disconnected". A sparse file proves the check
        // happens on the metadata, not on the bytes: reading this would take
        // half a gigabyte of RAM, and the test would not finish quietly.
        let path = std::env::temp_dir().join(format!("aiui-oversize-{}.mp4", std::process::id()));
        let f = std::fs::File::create(&path).expect("create sparse file");
        let size = crate::media::MEDIA_FILE_CAP + 1;
        f.set_len(size).expect("grow sparsely");
        drop(f);

        let err = upload_media(
            &reqwest::Client::new(),
            &test_cfg(),
            "test-token",
            path.to_str().unwrap(),
            "mp4",
        )
        .await
        .expect_err("an oversize clip must not be uploaded");

        assert!(err.contains(&size.to_string()), "names the file size: {err}");
        assert!(
            err.contains(&crate::media::MEDIA_FILE_CAP.to_string()),
            "names the cap: {err}"
        );
        // Nothing was sent: a POST would have failed against the port-0
        // config and said so instead.
        assert!(!err.contains("POST /media"), "no request attempted: {err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn format_confirm_result_passes_reason_through() {
        // #202: `confirm` is the tool that gates destructive actions, so
        // "the user said no" and "we gave up" must not collapse into the same
        // answer. `confirmed` still defaults to false — the documented shape
        // stays stable — but the reason rides along.
        for reason in ["ttl_expired", "evicted", "channel_dropped", "host_exiting"] {
            let render = json!({
                "id": "d1", "cancelled": true, "result": null, "reason": reason
            });
            let out = tool_payload(format_confirm_result(render));
            assert_eq!(out["cancelled"], json!(true));
            assert_eq!(out["confirmed"], json!(false));
            assert_eq!(out["reason"], json!(reason), "reason {reason} must survive");
        }
    }

    #[test]
    fn format_confirm_result_invents_no_reason_for_a_user_cancel() {
        let render = json!({"id": "d1", "cancelled": true, "result": null});
        let out = tool_payload(format_confirm_result(render));
        assert_eq!(out["cancelled"], json!(true));
        assert_eq!(out["confirmed"], json!(false));
        assert!(out.get("reason").is_none(), "no reason invented: {out}");

        // …and a real confirmation must not pick one up either.
        let render = json!({
            "id": "d1",
            "cancelled": false,
            "result": {"confirmed": true},
            "reason": "host_exiting"
        });
        let out = tool_payload(format_confirm_result(render));
        assert_eq!(out["confirmed"], json!(true));
        assert!(out.get("reason").is_none(), "not a cancel: {out}");
    }

    /// #202: a single transport blip must not kill a live dialog. This pins
    /// the budget the poll loop in `render_dialog` consults on every failed
    /// `send()` — the loop itself does nothing but `continue` while this says
    /// yes and return `RenderError::Transport` once it says no.
    #[test]
    fn poll_retries_transient_transport_error() {
        let mut budget = PollBudget::new(DEFAULT_POLL_TTL_SECS);

        // One failed poll then a success: the dialog survives, and the
        // success wipes the slate so a flaky link never accumulates its way
        // to a false give-up.
        assert!(budget.may_retry(), "a single blip must be retried");
        assert_eq!(budget.consecutive(), 1);
        budget.on_success();
        assert_eq!(budget.consecutive(), 0);

        // N *consecutive* failures exhaust it — that is the only give-up.
        for i in 1..POLL_MAX_CONSECUTIVE_FAILURES {
            assert!(budget.may_retry(), "failure {i} is still within budget");
        }
        assert!(
            !budget.may_retry(),
            "the {POLL_MAX_CONSECUTIVE_FAILURES}th consecutive failure gives up"
        );
        assert_eq!(budget.consecutive(), POLL_MAX_CONSECUTIVE_FAILURES);
    }

    #[test]
    fn poll_stops_once_the_advertised_ttl_has_elapsed() {
        // The id from the 202 is only valid for `ttl_secs`; past that the slot
        // is gone on the companion side and retrying it just burns the budget.
        let mut budget = PollBudget::new(0);
        assert!(!budget.may_retry(), "an expired id must not be re-polled");
    }

    /// #202: `/ping` is unauthenticated and returns a static "pong" whatever
    /// the dialog registry is doing, and single-occupancy is gone anyway
    /// (`dialog::DIALOG_HARD_CAP`) — so slot contention can never be the cause
    /// of this error. The message told the agent to pick ONE cause and listed
    /// three impossible ones first, sending users to close windows that do not
    /// exist.
    #[test]
    fn unreachable_message_does_not_blame_dialog_occupancy() {
        let text = aiui_unreachable_result()["content"][0]["text"]
            .as_str()
            .expect("text content")
            .to_string();
        for phrase in [
            "only one dialog",
            "freed the slot",
            "parallel Claude session",
            "leftover aiui",
            "one dialog at",
        ] {
            assert!(
                !text.contains(phrase),
                "obsolete occupancy narrative back in aiui_unreachable_result: {phrase:?}"
            );
        }
        // The two causes that *can* silence /ping must both be named. The
        // local cause is spelled through OPEN_HINT (platform-specific since
        // #204: /Applications on macOS, the Start menu on Windows), so assert
        // the macOS form here and leave the per-target hint to
        // `unreachable_hint_matches_target_os`.
        #[cfg(target_os = "macos")]
        assert!(text.contains("/Applications"), "local cause missing: {text}");
        assert!(
            text.contains("Settings → Connections"),
            "remote cause missing: {text}"
        );
    }

    fn test_cfg() -> Arc<AppConfig> {
        Arc::new(AppConfig {
            token: "test-token".into(),
            config_dir: PathBuf::from("/nonexistent"),
            token_path: PathBuf::from("/nonexistent/token"),
            http_port: 0,
        })
    }

    /// MCP 2026-07-28 clients probe a stdio server with `server/discover`
    /// before anything else and fall back to the legacy `initialize`
    /// handshake on any error that is not a recognized modern error
    /// (spec: basic/versioning, "Backward Compatibility"). Our
    /// `-32601 method not found` IS that fallback signal — if a dispatcher
    /// refactor ever answers `server/discover` differently (or swallows
    /// unknown methods), modern clients lose the probe response and treat
    /// aiui as broken instead of falling back.
    #[tokio::test]
    async fn server_discover_probe_gets_method_not_found() {
        let (tx, _rx) = mpsc::channel(1);
        let err = dispatch(
            "server/discover",
            json!({}),
            &test_cfg(),
            &reqwest::Client::new(),
            &tx,
            &new_render_sink(),
        )
        .await
        .expect_err("server/discover must be rejected, not answered");
        assert_eq!(err.code, -32601);
    }

    /// The legacy handshake aiui speaks until the 2026-07-28 migration:
    /// protocol version pinned to 2025-06-18, and `instructions` present —
    /// the one hook that breaks the model's chat-first default. Losing
    /// either silently degrades every session.
    #[tokio::test]
    async fn initialize_advertises_legacy_protocol_and_instructions() {
        let (tx, _rx) = mpsc::channel(1);
        let res = dispatch(
            "initialize",
            json!({}),
            &test_cfg(),
            &reqwest::Client::new(),
            &tx,
            &new_render_sink(),
        )
        .await
        .expect("initialize must succeed");
        assert_eq!(res["protocolVersion"], "2025-06-18");
        assert!(!res["instructions"].as_str().unwrap_or("").is_empty());
    }

    // ---------- #193: cancellation + EOF ----------

    use std::sync::atomic::{AtomicBool, Ordering};

    /// Flips its flag when dropped — which for a spawned task means "aborted".
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A dispatch task that never finishes on its own, plus the flag that says
    /// whether it was torn down.
    fn parked_entry(render_id: Option<&str>) -> (InFlight, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(false));
        let flag_for_task = flag.clone();
        let task = tokio::spawn(async move {
            let _guard = DropFlag(flag_for_task);
            std::future::pending::<()>().await;
        });
        (
            InFlight {
                task,
                render_id: Arc::new(Mutex::new(render_id.map(String::from))),
            },
            flag,
        )
    }

    /// #193: `notifications/cancelled` — Esc in Claude Code — was dropped with
    /// every other id-less message, so the bridge kept polling a dialog the
    /// client had already forgotten and the window stayed up until the 2 h TTL.
    #[tokio::test]
    async fn cancelled_notification_aborts_in_flight_request() {
        let mut in_flight: HashMap<String, InFlight> = HashMap::new();
        let (one, one_aborted) = parked_entry(Some("render-1"));
        let (two, two_aborted) = parked_entry(Some("render-2"));
        in_flight.insert("1".into(), one);
        in_flight.insert("2".into(), two);

        let render_id = handle_cancelled(&mut in_flight, &json!({"requestId": 1}));

        // The render id comes back so the caller can DELETE it on the companion.
        assert_eq!(render_id.as_deref(), Some("render-1"));
        assert!(!in_flight.contains_key("1"), "entry is deregistered");
        // Let the aborted task actually unwind.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(one_aborted.load(Ordering::SeqCst), "task 1 was aborted");

        // Everything else is untouched.
        assert!(in_flight.contains_key("2"));
        assert!(!two_aborted.load(Ordering::SeqCst), "task 2 still running");

        // A cancel that races the response (unknown id) is a harmless no-op.
        assert!(handle_cancelled(&mut in_flight, &json!({"requestId": 99})).is_none());
        assert!(handle_cancelled(&mut in_flight, &json!({})).is_none());
        assert!(in_flight.contains_key("2"));
    }

    /// String and integer request ids both canonicalise through
    /// `Value::to_string()`, so a client that sends string ids is cancellable too.
    #[tokio::test]
    async fn cancelled_notification_matches_string_request_ids() {
        let mut in_flight: HashMap<String, InFlight> = HashMap::new();
        let (entry, _) = parked_entry(None);
        in_flight.insert(json!("req-7").to_string(), entry);
        assert!(handle_cancelled(&mut in_flight, &json!({"requestId": "req-7"})).is_none());
        assert!(in_flight.is_empty(), "the entry was found and removed");
    }

    /// #193: every dispatch task holds a clone of the writer channel's sender,
    /// so `rx.recv()` — and therefore `writer_task.await` — only ended once the
    /// last tool call did. With a dialog outstanding that is up to the 2 h TTL,
    /// and the process lingered with `lifetime::mcp_attach` still attached.
    #[tokio::test]
    async fn stdin_eof_returns_while_call_in_flight() {
        let in_flight: InFlightMap = Arc::new(Mutex::new(HashMap::new()));
        let (entry, aborted) = parked_entry(None);
        in_flight.lock().unwrap().insert("1".into(), entry);

        // An input that is already at EOF: the parent closed the pipe.
        let (client, server) = tokio::io::duplex(64);
        drop(client);

        let run = run_stdio_io(test_cfg(), server, tokio::io::sink(), in_flight.clone());
        tokio::time::timeout(WRITER_DRAIN_TIMEOUT * 2, run)
            .await
            .expect("run_stdio must return promptly on EOF, not wait on the call");

        assert!(in_flight.lock().unwrap().is_empty());
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(aborted.load(Ordering::SeqCst), "in-flight task was aborted");
    }

    /// #203: MCP's ping utility requires an empty result, and a sender may
    /// treat a failed ping as a stale connection and tear the session down —
    /// which is how a missing arm here presents: "Server disconnected", not
    /// "method not found". Exactly one named arm; the catch-all stays strict
    /// (see `server_discover_probe_gets_method_not_found`).
    #[tokio::test]
    async fn ping_returns_empty_result() {
        let (tx, _rx) = mpsc::channel(1);
        let res = dispatch(
            "ping",
            json!({}),
            &test_cfg(),
            &reqwest::Client::new(),
            &tx,
            &new_render_sink(),
        )
        .await
        .expect("ping must be answered, not rejected");
        assert_eq!(res, json!({}));
    }

    /// #203: the Python bridge defaulted `allow_other` to `true` while this
    /// one defaults it to `false`, so identical agent code showed a free-text
    /// box on a remote host and not locally. `false` is the settled value;
    /// pin the advertised schema so the two cannot drift apart again.
    #[test]
    fn ask_schema_defaults_allow_other_to_false() {
        let tools = tools_list();
        let ask = tools
            .as_array()
            .expect("tools_list is an array")
            .iter()
            .find(|t| t["name"] == "ask")
            .expect("ask tool is advertised");
        assert_eq!(
            ask["inputSchema"]["properties"]["allow_other"]["default"],
            json!(false)
        );
    }

    /// #203: an absent optional must not be posted as an explicit `null` —
    /// `#[serde(default)]` on `NotifyRequest` covers an absent key only, so a
    /// `null` body died in axum's extractor with a serde dump.
    #[test]
    fn notify_body_omits_absent_optionals() {
        let body = compact_object(
            &json!({"title": "x", "body": "y"}),
            &["title", "body", "subtitle", "sound"],
        );
        assert_eq!(body, json!({"title": "x", "body": "y"}));
        assert!(body.get("subtitle").is_none(), "absent key must not appear");
        assert!(body.get("sound").is_none(), "absent key must not appear");
    }

    /// An explicit `null` in the args is as good as absent — the agent meant
    /// "not given" either way.
    #[test]
    fn notify_body_drops_explicit_nulls() {
        let body = compact_object(
            &json!({"title": "x", "body": "y", "subtitle": null, "sound": null}),
            &["title", "body", "subtitle", "sound"],
        );
        assert_eq!(body, json!({"title": "x", "body": "y"}));
    }

    /// #203: the companion answers a bad `/notify` with a structured
    /// `{"error","detail"}`; the agent should get the sentence, not the JSON.
    #[test]
    fn error_detail_prefers_the_structured_message() {
        assert_eq!(
            error_detail(r#"{"error":"invalid_request","detail":"title must not be empty"}"#),
            "title must not be empty"
        );
    }

    /// Anything that isn't a `{detail: str}` object passes through verbatim —
    /// a plain-text body from a proxy or an older companion must not vanish.
    #[test]
    fn error_detail_falls_back_to_the_raw_body() {
        assert_eq!(error_detail("plain text failure"), "plain text failure");
        assert_eq!(error_detail(r#"{"error":"nope"}"#), r#"{"error":"nope"}"#);
        assert_eq!(error_detail(""), "");
    }

    /// Every string the agent actually receives — the session `instructions`,
    /// the prompt bodies, both list responses and the two diagnostic blocks.
    /// Internal `///`/`//` comments are deliberately absent: they never reach
    /// a model.
    fn agent_facing_texts() -> Vec<(&'static str, String)> {
        vec![
            ("INSTRUCTIONS", INSTRUCTIONS.to_string()),
            ("UPLOAD_PROMPT", UPLOAD_PROMPT.to_string()),
            ("UPDATE_PROMPT", UPDATE_PROMPT.to_string()),
            ("VERSION_PROMPT", VERSION_PROMPT.to_string()),
            ("HEALTH_PROMPT", HEALTH_PROMPT.to_string()),
            ("TEST_DIALOG_PROMPT", TEST_DIALOG_PROMPT.to_string()),
            ("REMOTES_PROMPT", REMOTES_PROMPT.to_string()),
            ("tools_list", tools_list().to_string()),
            ("prompts_list", prompts_list().to_string()),
            (
                "aiui_unreachable_result",
                aiui_unreachable_result().to_string(),
            ),
        ]
    }

    /// #204: aiui shipped on Windows in v0.10.1, but the strings injected into
    /// the agent's context still described a Mac-only product — so an agent
    /// told a Windows user to "open aiui from /Applications", with the model's
    /// authority behind it. Agent-facing text must name no platform; the one
    /// place a concrete instruction is unavoidable is `OPEN_HINT`, which is
    /// chosen at compile time (see `unreachable_hint_matches_target_os`).
    #[test]
    fn agent_facing_text_is_platform_neutral() {
        for (label, text) in agent_facing_texts() {
            for needle in ["Mac", "macOS"] {
                assert!(
                    !text.contains(needle),
                    "{label} still says {needle:?} to the agent"
                );
            }
            // `/Applications` survives only inside the macOS `OPEN_HINT`.
            #[cfg(not(target_os = "macos"))]
            assert!(
                !text.contains("/Applications"),
                "{label} points a non-macOS user at /Applications"
            );
        }
    }

    // ---------- upload helpers (#146, covered by #211) ----------
    //
    // The Python bridge pins the same contract in
    // `python/tests/test_upload.py`. This bridge is the one every local
    // Claude Desktop / Claude Code user runs, and `safe_base_name` is the
    // only barrier stopping a companion-supplied `x-aiui-filename` from
    // escaping `target_dir` in `do_upload`.

    #[test]
    fn safe_base_name_strips_directories() {
        assert_eq!(safe_base_name("report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(
            safe_base_name("/Users/me/Downloads/report.pdf").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(safe_base_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(safe_base_name("  spaced.txt  ").as_deref(), Some("spaced.txt"));
    }

    #[test]
    fn safe_base_name_rejects_empty_and_dots() {
        for raw in ["", ".", "..", "/", "   "] {
            assert_eq!(safe_base_name(raw), None, "{raw:?} must not name a file");
        }
    }

    /// `Path::file_name()` is platform-dependent: a backslash is a path
    /// separator on Windows and an ordinary filename byte everywhere else,
    /// so the *same* header produces two different names on the two
    /// platforms aiui ships. Both are safe — neither escapes `target_dir` —
    /// but the divergence is real and belongs in writing rather than in
    /// someone's memory.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn safe_base_name_keeps_backslashes_off_windows() {
        assert_eq!(safe_base_name("..\\win.txt").as_deref(), Some("..\\win.txt"));
    }

    /// NOTE: compiled but never executed in CI — the Windows leg runs
    /// `cargo test --lib --no-run` (#141). It documents the divergence and
    /// starts running the day that is fixed.
    #[cfg(target_os = "windows")]
    #[test]
    fn safe_base_name_strips_backslash_directories_on_windows() {
        assert_eq!(safe_base_name("..\\win.txt").as_deref(), Some("win.txt"));
    }

    #[test]
    fn expand_dir_resolves_tilde_and_rejects_relative() {
        assert_eq!(expand_dir("~/x"), dirs::home_dir().map(|h| h.join("x")));
        assert_eq!(expand_dir("~"), dirs::home_dir());
        // Relative paths are a caller error: there is no stable cwd contract
        // to resolve them against, and guessing one writes the upload
        // somewhere the agent never named.
        assert_eq!(expand_dir("rel"), None);
        assert_eq!(expand_dir("./here"), None);
        assert_eq!(expand_dir(""), None);
    }

    /// Absoluteness is platform-dependent too: `"/abs"` is absolute on unix
    /// and *not* on Windows, so an identical `target_dir` is accepted on one
    /// shipping platform and rejected on the other.
    #[cfg(unix)]
    #[test]
    fn expand_dir_accepts_a_unix_absolute_path() {
        assert_eq!(expand_dir("/tmp/x"), Some(PathBuf::from("/tmp/x")));
    }

    /// NOTE: compiled but never executed in CI — see #141.
    #[cfg(windows)]
    #[test]
    fn expand_dir_accepts_a_windows_absolute_path() {
        assert_eq!(expand_dir(r"C:\tmp\x"), Some(PathBuf::from(r"C:\tmp\x")));
        // …and the unix-shaped one is not absolute here.
        assert_eq!(expand_dir("/abs"), None);
    }

    #[test]
    fn pct_decode_bytes_decodes_utf8_escapes() {
        assert_eq!(String::from_utf8(pct_decode_bytes("%C3%A4")).unwrap(), "ä");
        assert_eq!(String::from_utf8(pct_decode_bytes("plain.txt")).unwrap(), "plain.txt");
    }

    #[test]
    fn pct_decode_bytes_passes_malformed_escapes_through() {
        // The doc comment's promise: a mangled header degrades to a
        // slightly-odd name, never a lost upload.
        for raw in ["a%", "a%4", "%zz", "%"] {
            assert_eq!(
                String::from_utf8(pct_decode_bytes(raw)).unwrap(),
                raw,
                "{raw:?} must survive literally"
            );
        }
    }

    /// The cold-start hint is the one platform-specific sentence left, and it
    /// must follow the *build target* — this binary IS the companion, so the
    /// target is the user's OS. It must never follow
    /// `is_interactive_session()`, which only answers local-vs-SSH: that
    /// conflation is the bug.
    #[test]
    fn unreachable_hint_matches_target_os() {
        let text = aiui_unreachable_result()["content"][0]["text"]
            .as_str()
            .expect("text content")
            .to_string();

        #[cfg(target_os = "macos")]
        {
            assert!(text.contains("/Applications"), "macOS build: {text}");
            assert!(!text.contains("Start menu"), "macOS build: {text}");
        }
        #[cfg(target_os = "windows")]
        {
            assert!(text.contains("Start menu"), "Windows build: {text}");
            assert!(!text.contains("/Applications"), "Windows build: {text}");
        }

        // The context line reports local vs. SSH and claims no OS either way.
        assert!(
            text.contains("machine") || text.contains("remote dev host"),
            "context line lost: {text}"
        );
        assert!(!text.contains("local Mac"), "context line claims an OS: {text}");
    }

    #[test]
    fn filename_header_survives_the_encode_decode_round_trip() {
        // The encoder lives in http.rs and is tested there; only the decode
        // half was unguarded, which is exactly how the two halves drift
        // apart. Umlaut + space is the case users actually hit.
        let original = "Prüfung final.md";
        let encoded = crate::http::pct_encode_filename(original);
        assert!(encoded.is_ascii(), "header must stay ASCII: {encoded}");
        let decoded = String::from_utf8(pct_decode_bytes(&encoded)).expect("valid utf-8");
        assert_eq!(decoded, original);
        assert_eq!(safe_base_name(&decoded).as_deref(), Some(original));
    }

    // ---------- result shapes every agent parses ----------

    /// `structuredContent` and `content[0].text` must always be the same
    /// payload — a client reads one or the other, never both.
    fn assert_envelope_is_consistent(out: &Value) {
        let text = out["content"][0]["text"].as_str().expect("text content");
        let parsed: Value = serde_json::from_str(text).expect("content text is JSON");
        assert_eq!(parsed, out["structuredContent"], "envelope halves disagree");
    }

    #[test]
    fn confirm_result_reports_submit_and_cancel() {
        let out = format_confirm_result(json!({
            "id": "d1", "cancelled": false, "result": {"confirmed": true}
        }));
        assert_envelope_is_consistent(&out);
        assert_eq!(out["structuredContent"], json!({"cancelled": false, "confirmed": true}));

        // Cancel carries `result: null` — reading `confirmed` off it must
        // never be laundered into a "yes".
        let out = format_confirm_result(json!({"id": "d1", "cancelled": true, "result": null}));
        assert_envelope_is_consistent(&out);
        assert_eq!(out["structuredContent"], json!({"cancelled": true, "confirmed": false}));
    }

    #[test]
    fn confirm_result_survives_a_non_object_result() {
        let out = format_confirm_result(json!({"id": "d1", "cancelled": false, "result": "yes"}));
        assert_envelope_is_consistent(&out);
        assert_eq!(out["structuredContent"]["confirmed"], json!(false));
    }

    #[test]
    fn dialog_result_carries_the_values_and_the_cancelled_flag() {
        let out = format_dialog_result(json!({
            "id": "d1", "cancelled": false, "result": {"values": {"name": "Ada"}}
        }));
        assert_envelope_is_consistent(&out);
        assert_eq!(
            out["structuredContent"],
            json!({"values": {"name": "Ada"}, "cancelled": false})
        );
    }

    #[test]
    fn dialog_result_of_a_cancel_with_null_result_is_an_object() {
        // `result: null` is not an object, so the `cancelled` flag has
        // nowhere to go unless the fallback builds one. An agent parsing
        // `null["cancelled"]` is the alternative.
        let out = format_dialog_result(json!({"id": "d1", "cancelled": true, "result": null}));
        assert_envelope_is_consistent(&out);
        assert_eq!(out["structuredContent"], json!({"cancelled": true}));
    }

    #[test]
    fn dialog_result_of_a_non_object_result_does_not_panic() {
        for result in [json!("plain text"), json!(["a", "b"]), json!(7)] {
            let out = format_dialog_result(json!({
                "id": "d1", "cancelled": false, "result": result
            }));
            assert_envelope_is_consistent(&out);
            assert_eq!(out["structuredContent"], json!({"cancelled": false}));
        }
    }

    // ---------- two-bridge schema parity (#211) ----------

    /// The structural contract both bridges are held to. The Python bridge
    /// asserts against the same file in
    /// `python/tests/test_tool_schema_parity.py`, so an argument or default
    /// added on one bridge only turns red instead of drifting — which is how
    /// `ask.allow_other` ended up with two different defaults unnoticed.
    const TOOL_SCHEMA_FIXTURE: &str = include_str!("../../../schemas/tools-schema.json");

    /// Mirror of the normalisation documented in `schemas/tools-schema.json`.
    fn fixture_prop_type(p: &Value) -> String {
        if let Some(t) = p.get("type").and_then(|v| v.as_str()) {
            return if t == "integer" { "number".into() } else { t.into() };
        }
        if let Some(any) = p.get("anyOf").and_then(|v| v.as_array()) {
            let mut ts: Vec<String> = any
                .iter()
                .filter_map(|x| x.get("type").and_then(|v| v.as_str()))
                .filter(|t| *t != "null")
                .map(|t| if t == "integer" { "number".to_string() } else { t.to_string() })
                .collect();
            ts.sort();
            ts.dedup();
            if ts.len() == 1 {
                return ts.remove(0);
            }
            return ts.join("|");
        }
        "any".into()
    }

    /// A fixture `default` written as an object is a known divergence
    /// between the bridges; this side asserts its own value, so changing it
    /// here alone still fails.
    fn expected_default(entry: &Value) -> Option<Value> {
        match entry.get("default") {
            None => None,
            Some(Value::Object(m)) => Some(
                m.get("rust")
                    .cloned()
                    .expect("a divergence entry names both bridges"),
            ),
            Some(v) => Some(v.clone()),
        }
    }

    #[test]
    fn tools_list_matches_the_shared_schema_fixture() {
        let fixture: Value = serde_json::from_str(TOOL_SCHEMA_FIXTURE).expect("fixture is JSON");
        let expected = fixture["tools"].as_object().expect("fixture has tools");
        let listed = tools_list();
        let listed = listed.as_array().expect("tools_list is an array");

        let mut got: Vec<&str> = listed
            .iter()
            .map(|t| t["name"].as_str().expect("every tool is named"))
            .collect();
        got.sort_unstable();
        let mut want: Vec<&str> = expected.keys().map(String::as_str).collect();
        want.sort_unstable();
        assert_eq!(got, want, "tool set drifted from schemas/tools-schema.json");

        for tool in listed {
            let name = tool["name"].as_str().unwrap();
            let want = expected.get(name).unwrap();
            let schema = &tool["inputSchema"];

            let mut got_req: Vec<&str> = schema
                .get("required")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            got_req.sort_unstable();
            let mut want_req: Vec<&str> = want["required"]
                .as_array()
                .expect("required is a list")
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            want_req.sort_unstable();
            assert_eq!(got_req, want_req, "{name}: required[] drifted");

            let empty = serde_json::Map::new();
            let got_props = schema
                .get("properties")
                .and_then(|v| v.as_object())
                .unwrap_or(&empty);
            let want_props = want["properties"].as_object().expect("properties is a map");

            let mut got_names: Vec<&str> = got_props.keys().map(String::as_str).collect();
            got_names.sort_unstable();
            let mut want_names: Vec<&str> = want_props.keys().map(String::as_str).collect();
            want_names.sort_unstable();
            assert_eq!(got_names, want_names, "{name}: argument set drifted");

            for (prop, want_prop) in want_props {
                let got_prop = got_props.get(prop.as_str()).unwrap();
                assert_eq!(
                    fixture_prop_type(got_prop),
                    want_prop["type"].as_str().expect("type is a string"),
                    "{name}.{prop}: type drifted"
                );
                assert_eq!(
                    got_prop.get("default").cloned(),
                    expected_default(want_prop),
                    "{name}.{prop}: default drifted"
                );
            }
        }
    }
}
