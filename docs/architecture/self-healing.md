# RFC: Self-Healing Companion (event-driven, no idle ticks)

Status: draft
Origin: post-mortem of a 15h+ stuck-WebView incident on v0.3.0 (see
`/tmp/aiui-trace.log`, 2026-04-25). HTTP server stayed healthy while the
WebView event loop was effectively dead, so every `/render` call blocked
indefinitely with no automatic recovery.

## Goals

1. Failure modes that today require a manual GUI restart should self-heal.
2. No background polling. Idle companion = zero load except the OS-level
   event loop and the TCP listener. Every health/cleanup action must be
   triggered by a real cause. *A visible Settings window is a real cause;
   a hidden one is not* — see "The Settings status poll" below.
3. `/health` must reflect actual usability, not just "axum is up".

## Non-goals

- Reworking the dialog widget set or MCP tool surface.
- Replacing Tauri or the synchronous-feel of the MCP `form`/`ask`/`confirm`
  tools from the agent's perspective.

## Trigger taxonomy

Three legitimate trigger classes — anything outside these is a smell:

1. **Incoming request** (`/render`, `/health`, MCP tool call). Cheapest
   moment to verify liveness, because it's also the only moment liveness
   matters.
2. **OS lifecycle edges**: macOS sleep/wake, app foreground/background,
   Unix-domain-socket connect/disconnect. The OS is already firing these;
   we just listen.
3. **Sweep-as-side-effect**: each new action does a small bounded amount
   of cleanup before its own work. Cleanup load scales with activity, not
   wall time.

## Design changes

### 1. Asynchronous render with TTL

`POST /render` returns immediately with `{id, deadline}`. The result is
fetched separately (long-poll `GET /render/{id}` with a server-side max
wait, or SSE). Each registered dialog carries `created_at`; entries older
than TTL (default 5 min) are cancelled.

The agent-facing MCP wrapper keeps the synchronous look — it just polls
internally. From the agent's point of view, `form()` still blocks until the
user submits or until a structured timeout error comes back. **No more
indefinite hangs.**

### 2. Per-render ack contract (replaces continuous heartbeat)

After `app.emit("dialog:show", req)`, the Rust handler waits up to 500 ms
for `invoke("dialog_received", id)` from the frontend. If that ack arrives,
we proceed to wait for the user's response. If it doesn't:

- Conclude the WebView event loop is dead.
- Destroy the webview window and rebuild it via `WebviewWindowBuilder`.
- Re-emit `dialog:show` once.
- If the second ack also fails, return `ui_unreachable` to the caller and
  surface a notification.

Cost in steady state: zero. The check only runs when there's actually a
dialog to show.

### 3. Composite `/health` computed lazily

`/health` is a question, not a maintained state. On request:

- Synchronous mini-roundtrip to the frontend (`invoke("ui_ping")`,
  100 ms timeout).
- Read live counters from the dialog registry (orphan count, oldest age).
- Read mcp-stdio child count from the lifetime tracker.

Returned status is `ready` only if all three are sane. "Health green while
app is dead" becomes structurally impossible.

No background task maintains this. If nobody calls `/health`, nothing runs.

### 4. Opportunistic registry sweep

Every `DialogState::register()` call performs a bounded scan of the
HashMap and cancels entries older than TTL before inserting the new one.
Plus a hard cap (e.g. 16) — when exceeded, the oldest entry is evicted.

No reaper task. Cleanup work is paid by whoever is creating new work.

### 5. Edge-driven mcp-stdio child tracking

- Children terminate deterministically on stdin EOF — that's the standard
  MCP stdio contract; we just have to honour it (no keep-alive that
  outlives the parent).
- The GUI listens for socket disconnect events on `gui.sock` and removes
  the corresponding entry from the lifetime tracker immediately.
- On every new socket connect, the GUI does a sweep-on-attach: probe each
  tracked entry; any whose endpoint has EOF gets dropped.

No periodic ping, no timer. The cleanup edges fire from kernel events.

### 6. Idle-restart on natural occasion

Instead of a timer asking "is uptime > 24h?", piggy-back on incoming
requests:

- On `/render` arrival: if `gui_uptime > 24h && time_since_last_render > 10min`,
  recreate the webview window before serving the request.
- Hook `NSWorkspaceDidWakeNotification` to do the same after sleep.

Long-uptime drift gets flushed the moment it would actually matter.

### 7. Update check on lifecycle events, not on a 6h timer

Replace the recurring poll with checks at:

- GUI start (already there).
- After each successful `/render`.
- On wake-from-sleep.

These cluster around real user activity. A companion that sits unused for a
week does no update polling — which is fine, because nobody is using it.

### 8. The Settings status poll (#208)

The one `setInterval` that survives, and the conditions under which it is
allowed to. `Settings.svelte` polls the `status` command every 2 s so the
pane shows live tunnel state — but the setup window is *hidden* on close,
never destroyed (Invariant I2 in `lib.rs`), so its Svelte component stays
mounted for the life of the process and `onDestroy` never runs. Until #208
that meant one interval ticking forever: ~43k HTTP self-probes and
`pgrep`/`tasklist` child spawns per idle day, for a window nobody was
looking at. The principle above and the code said opposite things.

Two gates, so they now agree:

- **Visibility.** The poll runs only while the pane can be seen. It stops
  on `visibilitychange → hidden` and on window blur, and — because a
  hidden WKWebView can keep reporting `visibilityState: "visible"` — on a
  `setup:visibility` event Rust emits from the `CloseRequested` hide path
  and from every path that surfaces the window again. Reopening refreshes
  once immediately, then resumes ticking. An uninstall stops it for good.
- **A server-side TTL.** The two expensive halves of `status` — the
  authenticated HTTP self-probe and `is_claude_desktop_running()` (which
  spawns `pgrep` on macOS, `tasklist` on Windows) — sit behind a 15 s
  cache in `ExpensiveStatusCache`. A visible window therefore spawns at
  most four child processes a minute, while the cheap half (config flags,
  skill stat, remotes, tunnels, pending update) still answers every tick.
  User-triggered refreshes pass `force: true` and bypass the TTL, so a
  button never looks like it did nothing.

Note what was *not* done: the window is still hidden rather than
destroyed. Destroying it would unmount the component and make the symptom
disappear, at the cost of Invariant I2 — the process must keep serving the
HTTP endpoint and the lifetime socket after the window goes away.

## What this removes

- `setInterval(..., 6 * 60 * 60 * 1000)` for update polling.
- Any future temptation to add a `setInterval` for liveness, registry GC,
  or child sweeping. The Settings status poll is the single exception and
  is gated as described in §8; anything new needs the same two gates.
- The need for a manual "restart aiui" instruction in user-facing
  troubleshooting.

## What this adds

- `dialog_received(id)` Tauri command + per-render ack wait.
- `ui_ping` Tauri command for `/health`'s live probe.
- `WebviewWindowBuilder`-based recreate path on the main thread.
- TTL field + opportunistic sweep in `dialog::DialogState`.
- Socket-disconnect listener + sweep-on-attach in the lifetime tracker.

## Migration / staging

Implementable in two PRs without breaking the wire format:

- **PR 1**: ack-contract + WebView-recreate (#2), opportunistic sweep (#4),
  composite `/health` (#3). This alone resolves the observed incident
  class.
- **PR 2**: async `/render` with TTL (#1), edge-driven child tracking (#5),
  idle-restart on render (#6), lifecycle-driven update check (#7).

## Open questions

- Should the `ui_unreachable` error to the agent include the recreate
  attempt count, so the agent can decide whether to retry vs. surface to
  the user?
- For the async `/render` long-poll: pick a server-side max-wait that
  balances HTTP keep-alive friendliness with MCP client timeouts.
  Candidate: 60s, with the client looping until `deadline`.
- `NSWorkspaceDidWakeNotification` requires Tauri's macOS plugin or a
  small Objective-C bridge — verify which is lighter to maintain.
