<script lang="ts">
  import { _ } from "svelte-i18n";

  type ConfirmImage = {
    src: string; // data: URL, http(s) URL, or absolute / `~/` local path — bridge resolves before the spec hits the WebView
    alt?: string;
    max_height?: number;
  };
  type Spec = {
    kind: "confirm";
    title: string;
    message?: string;
    header?: string;
    destructive?: boolean;
    confirmLabel?: string;
    cancelLabel?: string;
    image?: ConfirmImage;
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
  {#if spec.image}
    <figure class="confirm-image" style={spec.image.max_height ? `max-height: ${spec.image.max_height}px` : ""}>
      <img src={spec.image.src} alt={spec.image.alt ?? spec.title} />
    </figure>
  {/if}
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

<style>
  .confirm-image {
    margin: 0;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    display: flex;
    justify-content: center;
    overflow: hidden;
  }
  .confirm-image img {
    max-width: 100%;
    max-height: 240px;
    height: auto;
    object-fit: contain;
  }
</style>
