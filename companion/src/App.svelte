<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { _ } from "svelte-i18n";
  import { onMount } from "svelte";
  import Ask from "./lib/widgets/Ask.svelte";
  import Form from "./lib/widgets/Form.svelte";
  import Confirm from "./lib/widgets/Confirm.svelte";
  import Settings from "./lib/Settings.svelte";
  import { checkForUpdates } from "./lib/updater";

  type DialogReq = { id: string; spec: any };

  let current = $state<DialogReq | null>(null);

  // Update checks are lifecycle-driven, not interval-driven. Triggers:
  //  • on mount (initial check at GUI start),
  //  • on `update:check` event from Rust (fired after each successful
  //    render — clusters around real user activity),
  //  • on window focus (covers wake-from-sleep and "user came back to the
  //    Mac" without needing an OS-level event hook).
  // A 30-minute cooldown debounces bursts so a chatty session doesn't
  // hammer the GitHub release endpoint.
  const UPDATE_COOLDOWN_MS = 30 * 60 * 1000;
  let lastUpdateCheck = 0;

  function maybeCheckForUpdates(reason: string) {
    const now = Date.now();
    if (now - lastUpdateCheck < UPDATE_COOLDOWN_MS) return;
    lastUpdateCheck = now;
    console.debug(`[aiui] update check (${reason})`);
    void checkForUpdates({ silent: true });
  }

  function onFocus() {
    maybeCheckForUpdates("window-focus");
  }

  onMount(() => {
    // Dialog event from Rust. We acknowledge receipt back to the Rust side
    // immediately so the `/render` handler knows the WebView event loop is
    // alive — this is the per-request liveness check that replaces the
    // need for any background UI heartbeat.
    const unDialog = listen<DialogReq>("dialog:show", (e) => {
      current = e.payload;
      void invoke("dialog_received", { id: e.payload.id });
    });

    // UI ping from Rust (used by /health to verify the event loop). We
    // pong back synchronously — the Rust side has a 100 ms timeout and a
    // missed pong is what flips /health to `degraded`.
    const unPing = listen<string>("ui:ping", (e) => {
      void invoke("ui_pong", { id: e.payload });
    });

    // Update-check trigger from Rust — fired after each successful render.
    // Frontend gates with the cooldown so we don't actually hit GitHub on
    // every render.
    const unUpdate = listen<string>("update:check", (e) => {
      maybeCheckForUpdates(`rust:${e.payload}`);
    });

    window.addEventListener("keydown", onKey);
    window.addEventListener("focus", onFocus);

    // Initial check on mount.
    maybeCheckForUpdates("startup");

    return async () => {
      (await unDialog)();
      (await unPing)();
      (await unUpdate)();
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("focus", onFocus);
    };
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") handleCancel();
  }

  async function handleSubmit(result: any) {
    if (!current) return;
    await invoke("dialog_submit", { id: current.id, result });
    current = null;
    await invoke("close_window");
  }

  async function handleCancel() {
    if (current) {
      await invoke("dialog_cancel", { id: current.id });
      current = null;
      await invoke("close_window");
    }
  }
</script>

<main class="container">
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
        <div class="stack">
          <p class="title">{$_("dialog.unknown_kind", { values: { kind: current.spec.kind } })}</p>
          <pre>{JSON.stringify(current.spec, null, 2)}</pre>
          <div class="footer">
            <button onclick={handleCancel}>{$_("dialog.close")}</button>
          </div>
        </div>
      {/if}
    {/key}
  {:else}
    <Settings />
  {/if}
</main>
