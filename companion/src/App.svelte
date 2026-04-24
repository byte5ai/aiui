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

  // Re-check for updates every 6 h in long-running sessions. The initial
  // startup check alone doesn't cut it: aiui lives for the whole Claude
  // Desktop session (often multiple days), so without a periodic timer a
  // user sitting on an outdated build never sees the prompt. Silent —
  // `checkForUpdates` only surfaces UI when an update is actually available.
  const UPDATE_POLL_MS = 6 * 60 * 60 * 1000;
  let updateTimer: number | undefined;

  onMount(() => {
    const un = listen<DialogReq>("dialog:show", (e) => {
      current = e.payload;
    });
    window.addEventListener("keydown", onKey);
    // Startup check + recurring poll.
    void checkForUpdates({ silent: true });
    updateTimer = window.setInterval(() => {
      void checkForUpdates({ silent: true });
    }, UPDATE_POLL_MS);
    return async () => {
      (await un)();
      window.removeEventListener("keydown", onKey);
      if (updateTimer !== undefined) window.clearInterval(updateTimer);
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
  {:else}
    <Settings />
  {/if}
</main>
