<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { _ } from "svelte-i18n";
  import { onMount } from "svelte";
  import Ask from "./widgets/Ask.svelte";
  import Form from "./widgets/Form.svelte";
  import Confirm from "./widgets/Confirm.svelte";

  type DialogReq = { id: string; spec: any; ttl_secs?: number };

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
    // Dialog event from Rust. We acknowledge receipt back to the Rust
    // side immediately so the `/render` handler knows the WebView event
    // loop is alive — this is the per-request liveness check that
    // replaces the need for any background UI heartbeat. Backend emits
    // this event with `emit_to("dialog", ...)`, so the setup window
    // never sees it.
    const dialogPromise = listen<DialogReq>("dialog:show", (e) => {
      current = e.payload;
      void invoke("dialog_received", { id: e.payload.id });
      scheduleTtl(e.payload.ttl_secs, e.payload.id);
    });

    // UI ping from Rust (used by /health to verify the event loop). We
    // pong back synchronously — the Rust side has a 100 ms timeout and
    // a missed pong is what flips /health to `degraded`.
    const pingPromise = listen<string>("ui:ping", (e) => {
      void invoke("ui_pong", { id: e.payload });
    });

    window.addEventListener("keydown", onKey);

    // Window-close (native red X / ⌘W) is owned by Rust as of v0.4.46
    // (on_window_event): it cancels any in-flight dialog and lets the
    // window close, and the `/render` handler destroys the window on
    // every terminal outcome. We deliberately no longer register a
    // frontend `onCloseRequested` here. The 0.4.45 version called
    // `event.preventDefault()` and then, if its cancel/close path failed
    // (empty/stale dialog state), left the window stranded — visible,
    // empty, and unclosable (Bug B, the 2026-05-29 overnight report).
    // Letting Rust own teardown removes that fragile round-trip.

    // Window-ready handshake (v0.4.30): tell the Rust render path
    // that our `dialog:show` listener is installed and we can safely
    // receive events. Without this, the backend would emit before
    // Tauri actually wired up the listener — the very-first render of
    // a fresh window would lose its event, hit the 500 ms ack timeout,
    // and the user would see a blank window. We await both subscribe
    // promises to ensure the listeners are *really* up before
    // signalling, not just queued.
    void Promise.all([dialogPromise, pingPromise]).then(() => {
      void invoke("dialog_window_ready");
    });

    return async () => {
      clearTtlTimers();
      (await dialogPromise)();
      (await pingPromise)();
      window.removeEventListener("keydown", onKey);
    };
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") handleCancel();
  }

  async function handleSubmit(result: any) {
    if (!current) return;
    clearTtlTimers();
    const id = current.id;
    current = null;
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
    caller. Issue #H-1 in v0.4.10 review. -->
  {#key current.id}
    {#if current.spec.kind === "ask"}
      <Ask spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
    {:else if current.spec.kind === "form"}
      <Form spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
    {:else if current.spec.kind === "confirm"}
      <Confirm spec={current.spec} onsubmit={handleSubmit} oncancel={handleCancel} />
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
       between window-show and the dialog:show event arriving. -->
  <main class="window-shell">
    <div class="idle"></div>
  </main>
{/if}

<style>
  .idle {
    min-height: 80px;
  }

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
