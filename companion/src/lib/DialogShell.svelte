<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { _ } from "svelte-i18n";
  import { onMount } from "svelte";
  import Ask from "./widgets/Ask.svelte";
  import Form from "./widgets/Form.svelte";
  import Confirm from "./widgets/Confirm.svelte";

  type DialogReq = { id: string; spec: any };

  let current = $state<DialogReq | null>(null);

  onMount(() => {
    // Dialog event from Rust. We acknowledge receipt back to the Rust
    // side immediately so the `/render` handler knows the WebView event
    // loop is alive — this is the per-request liveness check that
    // replaces the need for any background UI heartbeat. Backend emits
    // this event with `emit_to("dialog", ...)`, so the setup window
    // never sees it.
    const unDialog = listen<DialogReq>("dialog:show", (e) => {
      current = e.payload;
      void invoke("dialog_received", { id: e.payload.id });
    });

    // UI ping from Rust (used by /health to verify the event loop). We
    // pong back synchronously — the Rust side has a 100 ms timeout and
    // a missed pong is what flips /health to `degraded`.
    const unPing = listen<string>("ui:ping", (e) => {
      void invoke("ui_pong", { id: e.payload });
    });

    window.addEventListener("keydown", onKey);

    return async () => {
      (await unDialog)();
      (await unPing)();
      window.removeEventListener("keydown", onKey);
    };
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") handleCancel();
  }

  async function handleSubmit(result: any) {
    if (!current) return;
    const id = current.id;
    current = null;
    await invoke("dialog_submit", { id, result });
    await invoke("close_window");
  }

  async function handleCancel() {
    if (current) {
      const id = current.id;
      current = null;
      await invoke("dialog_cancel", { id });
      await invoke("close_window");
    } else {
      // No dialog yet — the user closed an empty dialog window. Just
      // close it.
      await invoke("close_window");
    }
  }
</script>

<!-- App.svelte provides the outer container with Apple-HIG title-bar
     padding and the drag region. We render straight into it. -->
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
  <!-- Brief idle state — only visible during the few hundred ms
       between window-show and the dialog:show event arriving. -->
  <div class="idle"></div>
{/if}

<style>
  .idle {
    min-height: 80px;
  }
</style>
