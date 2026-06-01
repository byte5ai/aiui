<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { _ } from "svelte-i18n";
  import { onMount } from "svelte";
  import Ask from "./widgets/Ask.svelte";
  import Form from "./widgets/Form.svelte";
  import Confirm from "./widgets/Confirm.svelte";
  import Gallery from "./widgets/Gallery.svelte";

  type DialogReq = {
    id: string;
    spec: any;
    ttl_secs?: number;
    // Multi-window (Step 4, I8): caller-set session label + remote-injected
    // origin host, shown in the window chrome so the user can tell which
    // session this dialog belongs to when several are open at once.
    session?: string;
    session_origin?: string;
  };

  let current = $state<DialogReq | null>(null);

  // TTL warning state. The backend sets `DIALOG_TTL` (currently 2 h)
  // and sends it along in `dialog:show`; we surface countdown banners
  // 15 min and 2 min before expiry, then auto-cancel a few seconds
  // before the backend sweep so the user's session reliably ends with
  // a "Zeit abgelaufen — Eingaben verworfen" close instead of a stale
  // dialog. v0.4.41. All timer state is per-dialog and reset on every
  // new `dialog:show` event, so a second dialog after the first
  // submitted starts with fresh banners and countdowns.
  let yellowBanner = $state(false);
  let yellowDismissed = $state(false);
  let redBanner = $state(false);
  let remainingSecs = $state<number | null>(null);
  let ttlTimers: ReturnType<typeof setTimeout>[] = [];
  let countdownInterval: ReturnType<typeof setInterval> | null = null;
  // ID of the dialog whose timers are currently scheduled. We snapshot
  // it on every scheduled callback so a timer that fires AFTER the
  // dialog has been replaced (or already submitted) cannot bleed into
  // the next dialog — classic race-on-rebind hazard with setTimeout.
  let ttlDialogId: string | null = null;

  function clearTtlTimers() {
    for (const t of ttlTimers) clearTimeout(t);
    ttlTimers = [];
    if (countdownInterval !== null) {
      clearInterval(countdownInterval);
      countdownInterval = null;
    }
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
  function scheduleTtl(ttl_secs: number | undefined, dialogId: string) {
    clearTtlTimers();
    if (!ttl_secs || ttl_secs <= 0) return;
    ttlDialogId = dialogId;

    const YELLOW_LEAD_SECS = 15 * 60;
    const RED_LEAD_SECS = 2 * 60;
    // Auto-cancel 5 seconds before the backend sweep so the dialog
    // ends cleanly on the frontend side first; the still-running
    // `/render` HTTP call then returns the cancellation to the agent.
    // Without this lead, the backend's TTL_EXPIRED sweep races the
    // user's last-second submit.
    const AUTO_CANCEL_LEAD_SECS = 5;

    const yellowDelayMs = (ttl_secs - YELLOW_LEAD_SECS) * 1000;
    const redDelayMs = (ttl_secs - RED_LEAD_SECS) * 1000;
    const cancelDelayMs = Math.max(0, (ttl_secs - AUTO_CANCEL_LEAD_SECS) * 1000);

    if (yellowDelayMs > 0) {
      ttlTimers.push(
        setTimeout(() => {
          if (ttlDialogId !== dialogId) return;
          yellowBanner = true;
          startCountdown(YELLOW_LEAD_SECS, dialogId);
        }, yellowDelayMs),
      );
    } else {
      // Edge case: TTL already <= 15 min on arrival. Show yellow now,
      // start the countdown from whatever's left.
      yellowBanner = true;
      startCountdown(ttl_secs, dialogId);
    }

    if (redDelayMs > 0) {
      ttlTimers.push(
        setTimeout(() => {
          if (ttlDialogId !== dialogId) return;
          redBanner = true;
          // Red overrides yellow's countdown: tighter cadence, no
          // dismiss button.
          startCountdown(RED_LEAD_SECS, dialogId);
        }, redDelayMs),
      );
    }

    ttlTimers.push(
      setTimeout(() => {
        if (ttlDialogId !== dialogId) return;
        // Auto-cancel — same code path as the ESC key / Cancel button.
        void handleCancel();
      }, cancelDelayMs),
    );
  }

  function startCountdown(initialSecs: number, dialogId: string) {
    if (countdownInterval !== null) {
      clearInterval(countdownInterval);
      countdownInterval = null;
    }
    remainingSecs = initialSecs;
    countdownInterval = setInterval(() => {
      if (ttlDialogId !== dialogId) {
        // Race guard: dialog already replaced, stop counting for it.
        if (countdownInterval !== null) {
          clearInterval(countdownInterval);
          countdownInterval = null;
        }
        return;
      }
      if (remainingSecs !== null && remainingSecs > 0) {
        remainingSecs -= 1;
      } else if (countdownInterval !== null) {
        clearInterval(countdownInterval);
        countdownInterval = null;
      }
    }, 1000);
  }

  function formatRemaining(secs: number): string {
    const safe = Math.max(0, secs);
    const m = Math.floor(safe / 60);
    const s = safe % 60;
    return `${m}:${String(s).padStart(2, "0")}`;
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
        current = req;
        scheduleTtl(req.ttl_secs, req.id);
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
        outcomes = await invoke("write_dialog_targets", { id, values });
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
      ⏱ {$_("dialog.ttl.yellow", { values: { countdown: remainingSecs !== null ? formatRemaining(remainingSecs) : "—" } })}
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
      <Form spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
    {:else if current.spec.kind === "confirm"}
      <Confirm spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
    {:else if current.spec.kind === "gallery"}
      <Gallery spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
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
  .ttl-banner.yellow {
    background: color-mix(in srgb, var(--warning, #f3c623) 28%, var(--bg, #fff));
    color: color-mix(in srgb, var(--warning, #f3c623) 70%, var(--fg, #000));
    border-bottom: 1px solid color-mix(in srgb, var(--warning, #f3c623) 60%, transparent);
  }
  .ttl-banner.red {
    background: color-mix(in srgb, var(--danger, #d64545) 22%, var(--bg, #fff));
    color: color-mix(in srgb, var(--danger, #d64545) 80%, var(--fg, #000));
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
