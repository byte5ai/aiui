# aiui Stabilization Plan (locked spec — guards against drift)

Status: Steps 1–4 implemented (Refs #137, v0.5.0). Step 4's tunnel was settled
empirically (2026-05-30): aiui-dedicated is correct and already in place — no
refactor needed; piggyback is impossible (Claude Desktop provides no reverse
forward). Per-step implementation records are inline under each step. The only
non-code residual is the remote-path integration harness (cross-cutting, below)
and a UX nicety (auto-discovering Code-tab remotes — needs Claude-Desktop
support).
Origin: root-cause analysis 2026-05-29 (Opus 4.8 code analysis + independent
Codex diagnosis, convergent; external validation of the three pivotal facts).

The instability is not a bug-swarm but **one architectural fault with many
symptoms**: aiui fuses three components with different lifecycles into one
binary whose lifetime hangs on a fragile proxy (count of mcp-stdio children
attached to a socket + 60 s grace) instead of on the real signal (is the host
Claude Desktop alive). The remote path additionally lacks the resurrection and
cold-start retry the local path has.

This plan is measured against the invariants below. Every change either
establishes an invariant or is out of scope.

## Invariants (the contract)

- **I1 — Host planned-exit ONLY in three cases:** (a) Claude Desktop terminates,
  (b) aiui is uninstalled, (c) update-restart. *Every other process exit is a
  crash* — logged as such, never traced as a clean shutdown.
- **I2 — Window close ≠ process exit.** Red X / ⌘W / "Beenden" on any window =
  hide window (+ demote to Accessory if no dialog remains). Never `app.exit`.
- **I3 — Tunnel owner = the Mac (SSH client), always.** Structurally forced:
  Mac→remote reachability is the precondition of the whole setup; remote→Mac is
  never guaranteed. The remote bridge is a passive forwarder; only the Mac can
  establish/repair the tunnel.
- **I4 — Remote bridge planned-exit ONLY:** stdin-EOF (its Claude Code session
  ended) or deregistration. *Never* killed from the Mac to force a version.
- **I5 — Nothing kills a process / tunnel / bridge that may hold an in-flight
  request.** Graceful drain before any teardown.
- **I6 — Local and remote bridges have identical resilience semantics**
  (cold-start poll, async-render polling, progress notifications, error
  classification, timeouts).
- **I7 — Every render resolves to exactly one terminal outcome** (kept from
  current behaviour).
- **I8 — Multi-window:** N concurrent dialogs allowed; **each window carries a
  human-legible session identifier** so the user can tell which session a dialog
  belongs to.

## Step 1 — Host lifetime invariant (decided; highest leverage, lowest risk)

Files: `lifetime.rs`, `lib.rs`.

Single exit authority. Introduce one predicate consulted by every
exit-candidate path:

    fn host_should_exit(reason) -> bool
      = explicit_uninstall_or_update  // cases (b)/(c): set by quit_app / updater
        || !setup::is_claude_desktop_running()  // case (a): the real Wirt signal

- **`lifetime::make_shutdown_watcher` (grace-expired):** keep the *edge* (last
  child disconnected) but, on that edge, consult `is_claude_desktop_running()`
  (already exists, `pgrep -f /Applications/Claude.app/`). CD alive → **do not
  arm grace, do not exit** (CD merely dropped/restarted the aiui MCP server, or
  Cowork churn). CD gone → short grace (≤5 s) then exit. This reuses an existing
  edge + an existing helper → **no objc bridge, no continuous poll tick.**
  - When CD quits it closes its children's stdin → EOF → they exit → disconnect
    → the edge fires → host exits. The edge therefore *does* fire on the only
    legitimate "Wirt endet" event.
  - Optional enhancement (later): an `NSWorkspaceDidTerminateApplication`
    observer (objc2) for instant detection. Not required for correctness.
- **Setup-window `CloseRequested` (lib.rs `on_window_event`):** `api.prevent_close()`
  + `window.hide()` + demote to Accessory. Remove the
  `setup-close-no-children → app.exit(0)` path entirely.
- **`RunEvent::ExitRequested` (lib.rs):** **default-deny** — `api.prevent_exit()`
  for *every* Tauri-initiated exit. Only the explicit paths (quit_app / updater
  restart) and the watcher's CD-gone exit may terminate. Removes the
  veto-by-child-count logic.
- **`LifetimeStats` child counter:** demoted to telemetry (`/health`) + the
  start-trigger only (`mcp_attach` first attach → `open --auto`). It **never**
  gates exit again.

No-regression: the grace-exit/idle-exit existed to reap stale state; that is now
covered by the existing `disk_version_if_stale` self-check + the housekeeping
sweeps, which stay.

## Step 2 — Remote bridge never killed to force a version (decided)

Files: `setup.rs`, `lib.rs`, `http.rs`, `python/.../server.py`.

- Remove `kill_remote_mcp_stdio` / `pkill -f 'aiui-mcp'` from the GUI-startup
  remote-pin loop (`lib.rs`) and from `resync_remote` / `add_remote` re-add.
  Patch the pin in `~/.claude.json`; it takes effect at the **next natural
  spawn**. A live session keeps its version until it ends.
- Keep an outbound kill ONLY for true deregistration (`remove_remote` /
  uninstall), and even there prefer config-removal + natural session end over a
  broad `pkill`. If a kill is kept, scope it precisely (never blunt `-f
  aiui-mcp`, which has cross-session blast radius — the remote twin of the
  0.4.42 Cowork-kill).
- Cooperative version floor (replaces external enforcement): add `wire_version`
  to `/version` + `/probe`. The bridge reads it on connect; on a hard
  incompatibility it returns a **structured tool error** ("incompatible aiui
  versions — restart this Claude Code session"), never gets killed, never
  crashes. Tolerate ordinary version skew (the wire contract is versioned and
  stable).

> **Implemented (2026-05-30, PR #137).** `kill_remote_mcp_stdio`
> (`ssh … pkill -f 'aiui-mcp'`) and all three of its callers — the GUI-startup
> remote-pin loop, `add_remote` re-add, and `resync_remote` — are deleted. The
> pin in `~/.claude.json` now takes effect at the next natural spawn; a live
> session keeps its version until it ends. `resync_remote` is re-pin-only.
> Deregistration (`remove_remote` / `uninstall_all`) was already
> config-removal + tunnel-stop, no kill — left as is. Cooperative floor:
> `WIRE_VERSION = 1` in `http.rs`, surfaced on `/version` + `/probe`; the
> Python bridge's `_check_wire_compat` reads it once per process and raises a
> structured "restart this session" error only on a hard wire mismatch (absent
> field → treated as v1, transient read errors tolerated). Tests: Rust 105
> green, Python 21 green (4 new wire-compat). The Rust bridge (`mcp.rs`) is the
> same binary as the companion, so it needs no floor check.

## Step 3 — Async render + bridge parity (decided; closes the ReadError class)

Files: `http.rs`, `dialog.rs`, `mcp.rs`, `python/.../server.py`.

> **Interim fix already landed (2026-05-30, with the Step-1 PR #137):** the
> acute 409-storm + stranded-empty-window pair was a *cancellation-safety* hole,
> not the full async gap. The synchronous `/render` handler parks on
> `timeout(DIALOG_TTL=2h, result_rx)` while the MCP client gives up far sooner
> (the local Rust bridge times out at 300 s). On client give-up Axum drops the
> handler future, and none of the explicit teardown ran → the registry entry
> leaked for 2 h (409 for every later render) and the surfaced window stranded
> empty. A `RenderGuard` (RAII) now cancels the entry + destroys the window on
> *any* drop, including the future-cancelled case. This makes the current
> handler cancellation-safe; the async-render protocol below still supersedes it
> (it removes the held connection entirely and is what properly supports long
> human fills without the client timing out at all).

- **Async `/render` (RFC point #1, never built):** `POST /render` registers the
  dialog and returns `{id, ttl}` immediately. New `GET /render/{id}` is a
  bounded long-poll (~25 s) returning `{pending}` or the terminal result.
  Removes the multi-minute open HTTP connection that any GUI/tunnel blip turns
  into ReadError.
- **Both bridges identical:** POST then loop `GET /render/{id}` until terminal
  or the client gives up; emit `notifications/progress` each iteration.
- **Python bridge to full parity (I6):**
  - Add the `wait_for_aiui` cold-start poll (`/ping` up to ~30 s) before
    posting — mirror the Rust bridge. Replaces the brittle single 3 s preflight.
  - Add progress notifications (FastMCP progress API — verify exact call).
  - Switch to the async-render polling loop above.
  - Classify errors precisely: TCP-refused (tunnel down) vs. connected-but-no-
    HTTP (= ReadError: tunnel up, Mac down) vs. 401 (token). Today the remote
    `ConnectError` branch is dead and ReadError gets a misleading message.

> **Implemented (2026-05-30, PR #137) — additive / backward-compatible.** Async
> render is opt-in via the `x-aiui-async` request header, *not* a replacement of
> the synchronous long-poll. This keeps the proven local path intact and means
> old bridges (which never send the header) keep working unchanged, so
> `WIRE_VERSION` stays 1.
> - **Companion (`http.rs`):** `POST /render` with the header registers +
>   surfaces the dialog, hands resolution to a detached task that fills an
>   `AsyncSlot`, and returns `202 {id, ttl_secs}`. New `GET /render/{id}`
>   poll-loops (200 ms ticks, bounded by `ASYNC_POLL_WINDOW` = 25 s) returning
>   the terminal result (drained once) / `{pending:true}` / `404`. Without the
>   header, the legacy synchronous path runs untouched. Resolution + window
>   teardown are shared by both via `resolve_dialog`. Resolved-but-uncollected
>   slots are swept at `DIALOG_TTL`.
> - **Both bridges (`mcp.rs`, `server.py`):** POST with the header, then loop
>   `GET /render/{id}` until terminal; each GET is bounded (40 s > server
>   window) so a blip costs one poll, never a held connection. Both fall back to
>   the synchronous result if the companion answers 200 instead of 202 (so a new
>   bridge works against an old companion too).
> - **Python parity (I6):** added `_wait_for_aiui` (`/ping` cold-start poll,
>   ~30 s), MCP progress notifications each pending poll (FastMCP
>   `Context.report_progress`, best-effort), the async polling loop, and an
>   explicit `httpx.ReadError` branch ("tunnel up, Mac not serving") distinct
>   from `ConnectError`. The Rust bridge already had cold-start + progress.
> - Tests: Rust 106 (slot lifecycle), Python 26 (+5: poll terminal/pending/404,
>   progress tick, cold-start tolerance). Not yet exercised end-to-end against a
>   live remote — integration harness is still the open cross-cutting item.

## Step 4 — Tunnel mechanism + multi-window (both resolved)

Files: `tunnel.rs`, `setup.rs`, `lib.rs`, `dialog.rs`, `http.rs`, frontend.

### Tunnel — DECISION SETTLED EMPIRICALLY (2026-05-30): aiui-dedicated, no change

The original deciding facts:
1. Does the SSH connection Claude Desktop opens to the remote carry a
   `RemoteForward 7777` from `~/.ssh/config`?
2. Is concurrent multi-session **on the same remote host** a requirement?

> **Measured on the Mac (client side), 2026-05-30 — corrects an earlier wrong
> inference.** A read-only probe of a live Claude-Desktop Code-tab session
> found:
> - **Fact (1) = NO.** Claude Desktop spawns `/usr/bin/ssh` *without* `-R` on
>   its command line, *without* a custom `-F` config, and `ssh -G <host>`
>   resolves **no `remoteforward`** for any host — there is no `RemoteForward`
>   in `~/.ssh/config`. CD does **not** provide a reverse forward.
> - **Fact (2) = YES** (user, 2026-05-30).
> - aiui **already** runs the dedicated path: two live
>   `ssh -N -T -R 7777:localhost:7777 … <host>` processes, parented by
>   `aiui.app … --auto`, one per registered remote. They work
>   (`ExitOnForwardFailure=yes` would have killed them on a bind clash).
>
> Both facts point to **aiui-dedicated**, which is also **what is already
> implemented**. **Piggyback is impossible**, not merely unchosen: there is no
> CD-provided forward to ride. (An earlier note here claimed CD provided the
> forward and proposed deleting the TunnelManager — that was based on reading
> the *remote's* aiui config, which is irrelevant: the **Mac** owns the
> tunnels. Deleting the TunnelManager would have broken all remote dialogs.)

- **aiui-dedicated** is correct and **needs no refactor.** The existing tunnel
  is already adequately hardened: `ExitOnForwardFailure=yes` (clean bind /
  collision handling), `ServerAliveInterval=30` + `ServerAliveCountMax=3`
  (dead-connection detection ≤90 s → ssh exits → reconnect), shared-forward
  detection (a second aiui / external owner of `:7777`), and the startup
  orphan-sweep. It needs a non-interactive auth path Mac→remote — satisfied for
  typical Code-tab users, who reach the remote via the *same* system `ssh` +
  agent that CD itself uses (measured: `forwardagent no`, `controlmaster
  false`, no ProxyJump → `BatchMode` succeeds).
- The spec's *"health = probe `/probe` through the tunnel, not 'ssh alive
  2 s'"* was written before crediting `ExitOnForwardFailure` + `ServerAlive`,
  which already make "process alive" a sound health proxy; the original
  ReadError driver (Mac HTTP not serving) is closed by **Step 1**. Adding a
  periodic SSH `/probe` loop would be real overhead for marginal gain — a
  quick-win deliberately **not** taken.
- The "running both in parallel" collision the plan feared was a hypothesis;
  the measurement shows only aiui's own `-NTR` and no competing manual
  `RemoteForward` (aiui strips those on `add_remote` anyway).

### Multi-window (I8)

- Drop single-occupancy: remove `try_register`'s "reject if any pending → 409".
  Allow N concurrent dialogs (registry already supports `DIALOG_HARD_CAP`).
- **One window per render**, window label = dialog id (replaces the single
  reused `DIALOG_WINDOW_LABEL`). Teardown keyed by id.
- **Session identifier (I8):**
  - Render spec gains a `session` field (string), set by the caller; tool
    wrappers (`mcp.rs`, `server.py`) gain a `session` param. Skill + tool
    descriptions instruct the agent to pass a short human label (project name
    etc.).
  - The **remote Python bridge auto-injects its `hostname`** (or the registered
    alias) as `session_origin`, so the user always sees which host a dialog came
    from even if the agent passes nothing (the Mac cannot distinguish remotes at
    `:7777` — all share one port — so origin must come from the caller side).
  - Window chrome (title bar / header chip) shows `session` + `session_origin`.
  - Fallback when the agent passes nothing: `session_origin` + short id.

> **Multi-window implemented (2026-05-30, PR #137); tunnel settled, no change
> needed** (see the empirical measurement above). The tunnel is already the
> correct aiui-dedicated mechanism and adequately hardened; piggyback is
> impossible (CD provides no forward). Step 4 is therefore complete bar the
> optional verified-`/probe` health polish, which was assessed and declined as
> marginal. The genuinely open, separate item is UX: aiui still requires the
> user to register each remote manually in its settings (with a working
> non-interactive ssh alias), independent of the Code-tab connection —
> auto-discovering CD's connected remotes would need Claude-Desktop support and
> is out of scope here.
>
> Multi-window itself is done, via a **pull model** rather than multiplying the
> old emit/ack/ready handshake per window:
> - `dialog.rs`: `try_register` (single-occupancy 409) → `register_dialog`
>   (N concurrent, evict-oldest only at `DIALOG_HARD_CAP`); the request payload
>   (spec + ttl + `session`/`session_origin`) is stored and pulled by id.
>   `cancel_all` removed (per-id cancel only — a blunt drain would kill other
>   sessions' live dialogs).
> - `http.rs`: `POST /render` builds a **fresh window labelled by the dialog id**
>   (`build_dialog_window`) and the whole emit/`dialog_window_ready`/ack-timeout/
>   reload-retry/idle-restart machinery is gone — the window pulls its spec via
>   `get_dialog_spec` on mount, so there's no event-before-listener race to
>   guard. Teardown is per-id.
> - `lib.rs`: `get_dialog_spec` command; per-id `destroy_dialog_window`;
>   Accessory demote when no dialog window remains; X-close cancels only the
>   closed window's own dialog; orphan-sweep is per-id.
> - Frontend `DialogShell.svelte`: reads its window label (= id), pulls the
>   spec, renders, and shows a fixed top-right **session chip** (`session` ·
>   `session_origin`), hidden when neither is set.
> - Bridges: `session` tool param on both (`mcp.rs`, `server.py`); the **Python
>   bridge auto-injects `socket.gethostname()` as `session_origin`** (I8
>   fallback for remotes sharing `:7777`).
> - Tests: Rust 102, Python 26, svelte-check 0 errors. **GUI behaviour is not
>   verifiable from the remote — needs validation on the Mac** (the
>   integration harness below is the right home for it).

## Cross-cutting — observability + test harness (makes "no regression" real)

- Promote the ad-hoc trace into an explicit named lifecycle state machine +
  event log (states: cold → serving → headless-idle → draining → exit(reason)).
- **Integration test harness for the remote path** — the layer with zero
  coverage today (only pure-function unit tests exist), which is *why* this has
  been whack-a-mole. Scenarios it must exercise before each release:
  tunnel-down, GUI-down-mid-call, update-mid-call, parallel sessions (same +
  different remotes), Claude-Desktop-quit, Claude-Desktop-restart.

## Sequencing & no-regression guardrail

Order: **1 → 2 → 3 → 4.** Each step is independently shippable and verifiable.
Steps 1–3 deliver the bulk of the stability gain and do **not** depend on the
tunnel decision.

For every mechanism this plan retires (grace-exit, remote `pkill`,
single-occupancy 409, and the dedicated tunnel if we choose piggyback): name the
original incident it guarded against and show the new model covers it **before**
deleting it. Each retired path was added for a real failure — the replacement
must demonstrably subsume that failure.

## Step 1 — implementation record (shipped under Refs #137)

Files touched: `lifetime.rs`, `lib.rs`, `http.rs`, `src/lib/updater.ts`.

Single exit authority is now real:

```
host_should_exit(explicit, cd_running) = explicit || !cd_running
```

`explicit` is an `ExitAuthority` latch (an `AtomicBool` in `lifetime.rs`,
`manage`d in `lib.rs`) set only by `quit_app` (uninstall), the HTTP `/update`
restart path (`http.rs`), and the frontend update-restart
(`authorize_exit_for_update`, called from `updater.ts` before `relaunch()`).
`cd_running` is `setup::is_claude_desktop_running()`. The predicate is a pure
function and unit-tested.

Wiring:

- **`lifetime::make_shutdown_watcher`** — keeps the last-child-disconnect edge
  as a *trigger only*. On the edge it arms a short grace (`SHUTDOWN_GRACE_SECS`,
  now 5 s) and then decides via the pure `grace_outcome(child_returned,
  cd_running)`: exit only if Claude Desktop is gone *and* no child returned;
  otherwise stay. No continuous poll — one `pgrep` per edge.
- **Setup-window `CloseRequested`** (`lib.rs`) — `api.prevent_close()` +
  `window.hide()` + Accessory demote. The `setup-close-no-children →
  app.exit(0)` path is gone.
- **`RunEvent::ExitRequested`** (`lib.rs`) — default-deny: `api.prevent_exit()`
  unless `host_should_exit`. The child-count / pending-dialog veto is gone.
- **`LifetimeStats`** — counter retained for `/health` telemetry and the
  `mcp_attach` start-trigger; it no longer reads into any exit decision.

### Verification mini-harness (Step 2 of the work order)

Pure decision functions are unit-tested in `lifetime.rs` so both invariants are
asserted without a live Claude Desktop or a running Tauri app — exactly the two
facts the runtime reads:

- `child_flap_with_claude_desktop_alive_stays` — `grace_outcome(false, true) =
  Stay`: the host survives a child flap while CD runs (the case the old 60 s
  grace got wrong).
- `claude_desktop_quit_with_no_child_exits` — `grace_outcome(false, false) =
  Exit`: the host follows the Wirt on CD-quit.
- `host_*` tests pin the three legitimate exits (uninstall, update, CD-gone).

### Prove-then-delete (Step 3 of the work order)

| Retired path | Original incident it guarded | New model that subsumes it |
|---|---|---|
| `grace-expired` (60 s after last child, child-count gated) | reap stale state when "nobody needs aiui" | the only legitimate "nobody needs us" signal is **Wirt gone** → `claude-desktop-gone` exit. Stale-binary / multi-instance reaping stays via `disk_version_if_stale` + housekeeping sweeps (untouched). The 60 s-on-child-count exit *was itself the bug* — it killed the host during Cowork churn / MCP re-spawn while CD was alive. |
| `setup-close-no-children → app.exit(0)` | let the app get out of the way when the user closed Settings and no children were attached (Issue #72 lineage) | I2: window close = hide + Accessory demote. The host stays headless with its Wirt; the Dock icon is dropped so "getting out of the way" no longer needs process death. Truly removing aiui is uninstall (`quit_app`, explicit). |
| `tauri-exit-requested-no-attached` (veto-by-child-count) | 0.4.42 lost the GUI ~18 ms after every dialog submit via Tauri's last-window-close ExitRequested; 0.4.44 added a child-count veto | default-deny `host_should_exit`: every Tauri-initiated exit is vetoed unless explicit uninstall/update or CD gone. The post-submit last-window-close exit is now unconditionally vetoed (CD alive, not explicit) → host survives. Child count no longer participates. |

### One deliberate deviation from the spec's prose

The spec text says "CD alive → **do not arm grace**". The implementation arms
the 5 s grace on *every* last-child-disconnect edge and gates the *exit* on a
fresh `is_claude_desktop_running()` probe at expiry. This is functionally
identical for the churn case (grace expires into `Stay`) but closes a race the
literal wording leaves open: if the edge fires while CD is mid-quit and `pgrep`
still matches a terminating helper, a "skip grace if CD looks alive" shortcut
would `Stay` with no further edge to re-trigger — stranding the host alive after
its Wirt is gone, a Step-2 ("host exits on CD-quit") regression. Invariant I1 is
preserved exactly: the exit is gated on `!is_claude_desktop_running()`; CD-alive
never exits.
