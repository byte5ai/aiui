<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { _ } from "svelte-i18n";
  import { onMount } from "svelte";
  import Ask from "./widgets/Ask.svelte";
  import Form from "./widgets/Form.svelte";
  import Confirm from "./widgets/Confirm.svelte";
  import Gallery from "./widgets/Gallery.svelte";
  import Compare from "./widgets/Compare.svelte";

  type DialogReq = {
    id: string;
    spec: any;
    ttl_secs?: number;
    /** Seconds actually LEFT at pull time, measured from `register_dialog`
     *  in Rust (#207). Absent on an older payload → we fall back to
     *  `ttl_secs` and behave as before. */
    remaining_secs?: number;
    // Multi-window (Step 4, I8): caller-set session label + remote-injected
    // origin host, shown in the window chrome so the user can tell which
    // session this dialog belongs to when several are open at once.
    session?: string;
    session_origin?: string;
  };

  let current = $state<DialogReq | null>(null);
  /** field name → absolute path, for the `target` approval line. Only filled
   *  for a local session; a bridge-served one keeps the raw `~/`-form. */
  let resolvedTargets = $state<Record<string, string>>({});

  // TTL warning state. The backend owns the clock: `register_dialog` stamps
  // `created_at` and `http.rs` arms `tokio::time::timeout(DIALOG_TTL)` right
  // after it, so the only honest countdown is one anchored to what Rust says
  // is LEFT. We surface countdown banners 15 min and 2 min before expiry,
  // then auto-cancel a few seconds before the backend sweep so the user's
  // session ends with a clean close instead of a stale dialog. v0.4.41.
  //
  // #207 replaced three latching `setTimeout`s and a decrementing counter
  // with one absolute deadline plus a 1 s repaint tick:
  //   - the old timers started when the WebView finished `get_dialog_spec`,
  //     not at registration, so window creation + a cold WebView start ate
  //     into (and could invert) the 5 s lead over the backend sweep;
  //   - the old counter decremented locally, and WebView timers are
  //     throttled in an occluded window and stop across system sleep, so the
  //     banner could read "1:58 left" with the real answer being zero;
  //   - the red timer re-seeded the count to exactly 2:00 when it fired.
  // Deriving every banner from one deadline on each tick removes all three.
  let yellowBanner = $state(false);
  let yellowDismissed = $state(false);
  let redBanner = $state(false);
  let remainingSecs = $state<number | null>(null);
  let deadlineMs: number | null = null;
  let tickInterval: ReturnType<typeof setInterval> | null = null;
  let resyncInterval: ReturnType<typeof setInterval> | null = null;
  // ID of the dialog whose timers are currently scheduled. We snapshot
  // it on every scheduled callback so a timer that fires AFTER the
  // dialog has been replaced (or already submitted) cannot bleed into
  // the next dialog — classic race-on-rebind hazard with setTimeout.
  let ttlDialogId: string | null = null;

  const YELLOW_LEAD_SECS = 15 * 60;
  const RED_LEAD_SECS = 2 * 60;
  // Auto-cancel 5 seconds before the backend sweep so the dialog ends
  // cleanly on the frontend side first; the still-running `/render` HTTP
  // call then returns the cancellation to the agent. Without this lead, the
  // backend's TTL_EXPIRED sweep races the user's last-second submit.
  const AUTO_CANCEL_LEAD_SECS = 5;
  // One IPC call per half-minute per open dialog, plus one whenever the
  // window becomes visible again.
  const RESYNC_EVERY_MS = 30_000;

  function clearTtlTimers() {
    if (tickInterval !== null) {
      clearInterval(tickInterval);
      tickInterval = null;
    }
    if (resyncInterval !== null) {
      clearInterval(resyncInterval);
      resyncInterval = null;
    }
    document.removeEventListener("visibilitychange", onVisibilityChange);
    deadlineMs = null;
    yellowBanner = false;
    yellowDismissed = false;
    redBanner = false;
    remainingSecs = null;
    ttlDialogId = null;
  }

  /**
   * Arm warning banners + auto-cancel for a freshly-arrived dialog.
   * Always called via `clearTtlTimers()` first so two consecutive
   * dialogs cannot leave stale timers running. Negative or missing
   * `ttl_secs` (older companion that doesn't send the field) → no
   * timers, no banners, current behaviour.
   */
  function scheduleTtl(req: DialogReq) {
    clearTtlTimers();
    const ttl = req.ttl_secs;
    if (!ttl || ttl <= 0) return;
    // An older payload without `remaining_secs` degrades to the previous
    // behaviour rather than to no countdown at all.
    const remaining = typeof req.remaining_secs === "number" ? req.remaining_secs : ttl;
    ttlDialogId = req.id;
    deadlineMs = Date.now() + Math.max(0, remaining) * 1000;
    tick();
    if (ttlDialogId !== req.id) return; // tick() may already have auto-cancelled
    tickInterval = setInterval(tick, 1000);
    resyncInterval = setInterval(() => void resyncDeadline(req.id), RESYNC_EVERY_MS);
    document.addEventListener("visibilitychange", onVisibilityChange);
  }

  /** The single place banner state and the countdown are derived. Idempotent:
   *  a missed tick (throttled window, sleep) costs nothing because the next
   *  one reads the deadline afresh rather than continuing a local count. */
  function tick() {
    if (deadlineMs === null || ttlDialogId === null) return;
    const left = Math.max(0, Math.round((deadlineMs - Date.now()) / 1000));
    remainingSecs = left;
    yellowBanner = left <= YELLOW_LEAD_SECS;
    redBanner = left <= RED_LEAD_SECS;
    if (left <= AUTO_CANCEL_LEAD_SECS) {
      // Auto-cancel — same code path as the ESC key / Cancel button.
      void handleCancel();
    }
  }

  /** Rebase the deadline on what Rust says is left. Rust's `Instant` is
   *  monotonic and on macOS/Windows may not advance while the machine sleeps,
   *  whereas `Date.now()` does — so without this the frontend would cancel
   *  EARLY after a sleep, the mirror image of the throttling bug. */
  async function resyncDeadline(dialogId: string) {
    if (ttlDialogId !== dialogId) return;
    try {
      const left = await invoke<number | null>("get_dialog_remaining", { id: dialogId });
      if (ttlDialogId !== dialogId) return;
      if (typeof left === "number") {
        deadlineMs = Date.now() + left * 1000;
        tick();
      }
      // `null` = the backend already resolved this dialog; Rust owns window
      // teardown in that case, so there is nothing for us to rebase.
    } catch (e) {
      console.error(`[aiui] get_dialog_remaining failed for ${dialogId}: ${e}`);
    }
  }

  function onVisibilityChange() {
    if (!document.hidden && ttlDialogId !== null) void resyncDeadline(ttlDialogId);
  }

  function formatRemaining(secs: number): string {
    const safe = Math.max(0, secs);
    const m = Math.floor(safe / 60);
    const s = safe % 60;
    return `${m}:${String(s).padStart(2, "0")}`;
  }

  /** The yellow banner's catalog string already carries the unit ("… min
   *  left"), so feeding it `m:ss` produced "About 15:00 min left". Whole
   *  minutes, rounded up so it never reads 0 while time remains. */
  function formatMinutes(secs: number): number {
    return Math.max(0, Math.ceil(secs / 60));
  }

  onMount(() => {
    // Multi-window pull model (Step 4): this window's label IS its dialog id.
    // Fetch our own render payload from Rust by that id — the frontend
    // initiates, so there's no `dialog:show` emit to race and no
    // ready-handshake to perform. If the dialog is already gone
    // (resolved/evicted before we mounted), close the window.
    const id = getCurrentWindow().label;
    void (async () => {
      try {
        const req = await invoke<DialogReq | null>("get_dialog_spec", { id });
        if (!req) {
          // Nothing to show — a stranded/already-resolved window. Close it.
          try {
            await invoke("close_window");
          } catch (e) {
            console.error(`[aiui] close_window (no spec) failed: ${e}`);
          }
          return;
        }
        // #207: resolve `~/` target paths BEFORE the first paint, so the
        // approval line never shows the raw form and then swap it out under
        // the user. Local sessions only — for a bridge-served one the write
        // happens on the agent's host, whose `$HOME` this app cannot know.
        if (!req.session_origin && collectTargetFields(req.spec).length > 0) {
          try {
            resolvedTargets = await invoke<Record<string, string>>("resolve_dialog_targets", {
              id: req.id,
            });
          } catch (e) {
            console.error(`[aiui] resolve_dialog_targets failed for ${req.id}: ${e}`);
          }
        }
        current = req;
        scheduleTtl(req);
        // Session identity (I8) is set as the native window title by Rust in
        // build_dialog_window — the frontend setTitle is permission-gated
        // (needs core:window:set-title), so we don't do it here.
      } catch (e) {
        console.error(`[aiui] get_dialog_spec failed for ${id}: ${e}`);
      }
    })();

    window.addEventListener("keydown", onKey);

    // Window-close (native red X / ⌘W) is owned by Rust (on_window_event):
    // it cancels THIS window's dialog by its id and lets the window close,
    // and the `/render` handler destroys the window on every terminal
    // outcome. We deliberately don't register a frontend `onCloseRequested`
    // — the 0.4.45 version's `preventDefault()` + failed close stranded
    // empty windows (Bug B). Letting Rust own teardown removes that race.

    return () => {
      clearTtlTimers();
      window.removeEventListener("keydown", onKey);
    };
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") handleCancel();
  }

  /** Fields carrying a `target` (file-write, issue #135), from flat `fields`
   *  and any `tabs[].fields`. Returns `{name, kind}` so the caller knows which
   *  values to write out and which (secret) to strip from the result. */
  function collectTargetFields(spec: any): { name: string; kind: string }[] {
    const out: { name: string; kind: string }[] = [];
    const scan = (fields: any) => {
      if (!Array.isArray(fields)) return;
      for (const f of fields) {
        if (f && f.target != null && typeof f.name === "string") {
          out.push({ name: f.name, kind: f.kind });
        }
      }
    };
    scan(spec?.fields);
    if (Array.isArray(spec?.tabs)) for (const t of spec.tabs) scan(t?.fields);
    return out;
  }

  /** Every `secret`-kind field, regardless of whether it carries a `target`
   *  (issue #186). The write-only contract is a property of the KIND: the
   *  docs say a secret's value is never returned, but the strip below used
   *  to key on `target`, so a target-less `secret` went back as plaintext. */
  function collectSecretFields(spec: any): string[] {
    const out: string[] = [];
    const scan = (fields: any) => {
      if (!Array.isArray(fields)) return;
      for (const f of fields) {
        if (f && f.kind === "secret" && typeof f.name === "string") out.push(f.name);
      }
    };
    scan(spec?.fields);
    if (Array.isArray(spec?.tabs)) for (const t of spec.tabs) scan(t?.fields);
    return out;
  }

  async function handleSubmit(result: any) {
    if (!current) return;
    clearTtlTimers();
    const id = current.id;
    const spec = current.spec;
    const sessionOrigin = current.session_origin;
    current = null;

    // Issue #135: write `target`-carrying fields to files on the agent's host.
    // The write is always a LOCAL file op on whichever aiui module sits on the
    // agent's host:
    //   - Local native-app session (`session_origin` absent): the app writes
    //     here, on the Mac, and strips secret values from the result so they
    //     never reach the bridge/agent.
    //   - Bridge-served session (`session_origin` set — remote SSH, or local
    //     uvx): the bridge on the agent's host does the local write + strip.
    //     We must NOT write or strip here, so the entered value reaches that
    //     bridge over the :7777 channel (never via the agent/LLM).
    // Form values live under `result.values` ({action, values:{name:val}}).
    const fieldValues: Record<string, any> = result?.values ?? {};
    const targets = sessionOrigin ? [] : collectTargetFields(spec);
    if (targets.length > 0) {
      const values: Record<string, string> = {};
      for (const t of targets) {
        const v = fieldValues[t.name];
        values[t.name] = v == null ? "" : String(v);
      }
      let outcomes: Record<string, any> = {};
      try {
        // Issue #177: hand the pressed action to the writer. The authoritative
        // decision whether it commits lives in Rust, resolved against the
        // stored spec — this is a convenience pass-through, not the guard.
        outcomes = await invoke("write_dialog_targets", {
          id,
          values,
          action: result?.action ?? null,
        });
      } catch (e) {
        console.error(`[aiui] write_dialog_targets failed for ${id}: ${e}`);
        // Synthesise a failure outcome so the agent is informed instead of
        // silently receiving nothing — and we can still strip secrets below.
        for (const t of targets) {
          outcomes[t.name] = { written: false, target: "", bytes: 0, error: String(e) };
        }
      }
      // Merge outcomes into result.values; strip raw secret values regardless
      // of write success so a secret can never leak even on the error path.
      for (const t of targets) {
        const outcome = outcomes[t.name] ?? {
          written: false,
          target: "",
          bytes: 0,
          error: "no outcome returned",
        };
        if (t.kind === "secret") {
          fieldValues[t.name] = outcome;
        } else {
          fieldValues[t.name] = { value: fieldValues[t.name], ...outcome };
        }
      }
    }

    // Issue #186: belt and braces for a `secret` that carries no `target` and
    // was therefore never in `targets`. A current companion rejects that
    // shape in validate_spec, but an older one in front of a newer bridge
    // would not — and the value must never reach the agent either way.
    //
    // Only for a LOCAL session. With `session_origin` set the plaintext must
    // still travel to the bridge over the :7777 channel, because the bridge
    // on the agent's host performs the write and does its own strip. Stripping
    // here would make it write the stringified outcome object into the user's
    // credential file and report success.
    if (!sessionOrigin) {
      for (const name of collectSecretFields(spec)) {
        const v = fieldValues[name];
        const alreadyStripped = v != null && typeof v === "object" && "written" in v;
        if (!alreadyStripped) {
          fieldValues[name] = {
            written: false,
            target: "",
            bytes: 0,
            error: "secret field has no target — value discarded, never returned",
          };
        }
      }
    }

    // v0.4.45 (Bug #3): never swallow the invoke result silently. If
    // dialog_submit fails the agent would otherwise hang forever with
    // no signal — at least surface it to the console for diagnosis.
    try {
      await invoke("dialog_submit", { id, result });
    } catch (e) {
      console.error(`[aiui] dialog_submit failed for ${id}: ${e}`);
    }
    try {
      await invoke("close_window");
    } catch (e) {
      console.error(`[aiui] close_window failed: ${e}`);
    }
  }

  async function handleCancel() {
    clearTtlTimers();
    if (current) {
      const id = current.id;
      current = null;
      try {
        await invoke("dialog_cancel", { id });
      } catch (e) {
        console.error(`[aiui] dialog_cancel failed for ${id}: ${e}`);
      }
    }
    // Always close the window — whether we just cancelled a live dialog
    // or the user closed an already-empty dialog window.
    try {
      await invoke("close_window");
    } catch (e) {
      console.error(`[aiui] close_window failed: ${e}`);
    }
  }
</script>

<!-- DialogShell is a thin host: it owns the per-window event listeners,
     the keyboard handler and the lifetime of the current dialog spec.
     The actual chrome (header / scroll / footer) is provided by the
     widget components themselves so they can put whatever sections they
     need into the scroll region and own their own button row. The shell
     just makes sure the WebView fills its window. -->

<!-- TTL warning banners. Two-stage: yellow at T-15min, red at T-2min.
     Position-fixed overlay so it never reflows the widget below it —
     content scrolls under the banner. v0.4.41. -->
{#if redBanner}
  <div class="ttl-banner red" role="alert" aria-live="assertive">
    <span class="ttl-banner-text">
      ⚠️ {$_("dialog.ttl.red", { values: { countdown: remainingSecs !== null ? formatRemaining(remainingSecs) : "—" } })}
    </span>
  </div>
{:else if yellowBanner && !yellowDismissed}
  <div class="ttl-banner yellow" role="status" aria-live="polite">
    <span class="ttl-banner-text">
      ⏱ {$_("dialog.ttl.yellow", { values: { countdown: remainingSecs !== null ? formatMinutes(remainingSecs) : "—" } })}
    </span>
    <button
      class="ttl-banner-dismiss"
      onclick={() => (yellowDismissed = true)}
      aria-label={$_("dialog.ttl.dismiss_aria")}
    >×</button>
  </div>
{/if}

{#if current}
  <!-- {#key current.id} forces a fresh widget instance for every new
    dialog, even when two consecutive renders are the same kind (e.g.
    two `confirm`s). Without it, Svelte recycles the component and
    stale field/checkbox/radio state from the previous dialog can bleed
    into the current one — silently sending wrong answers back to the
    caller. Issue #H-1 in v0.4.10 review. Session identity (I8) lives in
    the native title bar — set via setTitle in onMount — not in the work
    area, so it can never overlap dialog content. -->
  {#key current.id}
    {#if current.spec.kind === "ask"}
      <Ask spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
    {:else if current.spec.kind === "form"}
      <Form
        spec={current.spec}
        onsubmit={handleSubmit}
        oncancel={handleCancel}
        sessionOrigin={current.session_origin}
        {resolvedTargets}
      />
    {:else if current.spec.kind === "confirm"}
      <Confirm spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
    {:else if current.spec.kind === "gallery"}
      <Gallery spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
    {:else if current.spec.kind === "compare"}
      <Compare spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
    {:else}
      <main class="window-shell">
        <div class="window-scroll">
          <p class="title">{$_("dialog.unknown_kind", { values: { kind: current.spec.kind } })}</p>
          <pre>{JSON.stringify(current.spec, null, 2)}</pre>
        </div>
        <footer class="window-footer">
          <button onclick={handleCancel}>{$_("dialog.close")}</button>
        </footer>
      </main>
    {/if}
  {/key}
{:else}
  <!-- Brief idle state — only visible during the few hundred ms
       between window-show and the spec arriving. -->
  <main class="window-shell">
    <div class="idle"></div>
  </main>
{/if}

<style>
  .idle {
    min-height: 80px;
  }

  /* Session identity (I8) now lives in the native window title bar (set via
     setTitle in onMount), so there is no in-work-area chip/markup to style. */

  /* TTL countdown banner. Position-fixed so the widget below keeps
     its own three-zone (.window-shell) layout intact — content
     scrolls beneath the banner. Banner height is deliberately small
     so a typical small confirm/ask isn't completely covered. */
  .ttl-banner {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 14px;
    font-size: 12.5px;
    line-height: 1.3;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12);
  }
  .ttl-banner-text {
    flex: 1 1 auto;
    min-width: 0;
  }
  /* #207: both banners used to mix 70–80% of the accent colour into the
     label sitting on a wash of the same colour — ≈3.0:1 (yellow) and
     ≈4.4:1 (red) at 12.5px, and 600 weight at 12.5px is not WCAG "large
     text". 35% keeps the tone and lets `--fg` carry the legibility. The
     ramps themselves are untouched: `--warning`/`--danger` are used as
     plain foregrounds and as fills elsewhere, so the pairing is what
     changes, not the token. */
  .ttl-banner.yellow {
    background: color-mix(in srgb, var(--warning, #f3c623) 28%, var(--bg, #fff));
    color: color-mix(in srgb, var(--warning, #f3c623) 35%, var(--fg, #000));
    border-bottom: 1px solid color-mix(in srgb, var(--warning, #f3c623) 60%, transparent);
  }
  .ttl-banner.red {
    background: color-mix(in srgb, var(--danger, #d64545) 22%, var(--bg, #fff));
    color: color-mix(in srgb, var(--danger, #d64545) 35%, var(--fg, #000));
    border-bottom: 1px solid color-mix(in srgb, var(--danger, #d64545) 70%, transparent);
    font-weight: 600;
  }
  .ttl-banner-dismiss {
    flex: 0 0 auto;
    background: transparent;
    border: none;
    cursor: pointer;
    color: inherit;
    font-size: 16px;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 4px;
  }
  .ttl-banner-dismiss:hover {
    background: color-mix(in srgb, currentColor 12%, transparent);
  }
</style>
