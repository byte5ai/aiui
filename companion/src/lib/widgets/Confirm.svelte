<script lang="ts">
  import { _ } from "svelte-i18n";

  type Spec = {
    kind: "confirm";
    title: string;
    message?: string;
    header?: string;
    destructive?: boolean;
    confirmLabel?: string;
    cancelLabel?: string;
  };

  let { spec, onsubmit, oncancel }: { spec: Spec; onsubmit: (r: any) => void; oncancel: () => void } = $props();

  function confirm() {
    onsubmit({ confirmed: true });
  }
  function deny() {
    onsubmit({ confirmed: false });
  }
</script>

<div class="stack">
  {#if spec.header}<span class="chip">{spec.header}</span>{/if}
  <div>
    <p class="title">{spec.title}</p>
    {#if spec.message}<p class="subtitle">{spec.message}</p>{/if}
  </div>

  <div class="footer">
    <button onclick={deny}>{spec.cancelLabel ?? $_("dialog.confirm.no")}</button>
    <button class={spec.destructive ? "danger" : "primary"} onclick={confirm}
      >{spec.confirmLabel ?? $_("dialog.confirm.yes")}</button
    >
  </div>
</div>
