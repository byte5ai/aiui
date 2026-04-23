<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { _ } from "svelte-i18n";
  import { onMount } from "svelte";
  import Ask from "./lib/widgets/Ask.svelte";
  import Form from "./lib/widgets/Form.svelte";
  import Confirm from "./lib/widgets/Confirm.svelte";
  import Settings from "./lib/Settings.svelte";

  type DialogReq = { id: string; spec: any };

  let current = $state<DialogReq | null>(null);

  onMount(() => {
    const un = listen<DialogReq>("dialog:show", (e) => {
      current = e.payload;
    });
    window.addEventListener("keydown", onKey);
    return async () => {
      (await un)();
      window.removeEventListener("keydown", onKey);
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
