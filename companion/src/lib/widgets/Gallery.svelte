<script lang="ts">
  import { _ } from "svelte-i18n";

  type Action = {
    label: string;
    value: string;
    primary?: boolean;
    success?: boolean;
    destructive?: boolean;
  };
  type Item = {
    value: string; // stable id returned in the result
    src?: string; // image (data:/http/local→data) or video (data:/http; local video lands with the scp-transfer increment)
    alt?: string;
    label?: string;
    detail?: string; // short context shown beside the thumbnail
    max_height?: number;
  };
  type Spec = {
    kind: "gallery";
    title?: string;
    description?: string;
    header?: string;
    items: Item[];
    actions?: Action[]; // per-item decision buttons; default Approve / Revise / Skip
    comment?: boolean; // show a per-item comment field
    columns?: number; // grid columns; default responsive auto-fill
    submitLabel?: string;
    cancelLabel?: string;
  };

  let { spec, onsubmit, oncancel }: { spec: Spec; onsubmit: (r: any) => void; oncancel: () => void } =
    $props();

  const DEFAULT_ACTIONS: Action[] = [
    { label: "Approve", value: "approve", success: true },
    { label: "Revise", value: "revise" },
    { label: "Skip", value: "skip" },
  ];
  const actions = $derived(spec.actions && spec.actions.length ? spec.actions : DEFAULT_ACTIONS);

  // Per-item decision + comment, keyed by item value.
  let decisions = $state<Record<string, string>>({});
  let comments = $state<Record<string, string>>({});

  function pick(itemValue: string, actionValue: string) {
    // Toggle off if the same action is clicked again.
    decisions = { ...decisions, [itemValue]: decisions[itemValue] === actionValue ? "" : actionValue };
  }

  const decidedCount = $derived(Object.values(decisions).filter(Boolean).length);

  function isVideo(src: string | undefined): boolean {
    if (!src) return false;
    if (src.startsWith("data:video/")) return true;
    return /\.(mp4|mov|m4v|webm)(\?|#|$)/i.test(src);
  }

  function submit() {
    const out: Record<string, { decision: string; comment?: string }> = {};
    for (const it of spec.items) {
      const decision = decisions[it.value] || "";
      const comment = (comments[it.value] || "").trim();
      if (decision || comment) {
        out[it.value] = comment ? { decision, comment } : { decision };
      }
    }
    onsubmit({ decisions: out });
  }

  const cols = $derived(spec.columns && spec.columns > 0 ? spec.columns : 0);
</script>

<main class="window-shell">
  <div class="window-scroll">
    {#if spec.header}<span class="chip">{spec.header}</span>{/if}
    {#if spec.title}<p class="title">{spec.title}</p>{/if}
    {#if spec.description}<p class="subtitle">{spec.description}</p>{/if}

    <div
      class="gallery-grid"
      style={cols
        ? `grid-template-columns: repeat(${cols}, minmax(0, 1fr));`
        : "grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));"}
    >
      {#each spec.items as item (item.value)}
        <div class="gallery-item" class:decided={!!decisions[item.value]}>
          {#if item.src}
            <div class="gallery-thumb" style={item.max_height ? `max-height:${item.max_height}px` : ""}>
              {#if isVideo(item.src)}
                <!-- svelte-ignore a11y_media_has_caption -->
                <video src={item.src} controls preload="metadata"></video>
              {:else}
                <img src={item.src} alt={item.alt ?? item.label ?? item.value} />
              {/if}
            </div>
          {/if}
          {#if item.label}<div class="gallery-label">{item.label}</div>{/if}
          {#if item.detail}<div class="gallery-detail">{item.detail}</div>{/if}

          <div class="gallery-actions">
            {#each actions as a (a.value)}
              <button
                type="button"
                class="ga-btn"
                class:selected={decisions[item.value] === a.value}
                class:success={a.success}
                class:danger={a.destructive}
                class:primary={a.primary}
                onclick={() => pick(item.value, a.value)}
              >{a.label}</button>
            {/each}
          </div>

          {#if spec.comment}
            <input
              type="text"
              class="gallery-comment"
              placeholder={$_("dialog.gallery.comment_placeholder")}
              bind:value={comments[item.value]}
            />
          {/if}
        </div>
      {/each}
    </div>
  </div>

  <footer class="window-footer">
    <span class="gallery-count">{$_("dialog.gallery.decided", { values: { n: decidedCount, total: spec.items.length } })}</span>
    <button onclick={oncancel}>{spec.cancelLabel ?? $_("dialog.cancel")}</button>
    <button class="primary" onclick={submit}>{spec.submitLabel ?? $_("dialog.submit")}</button>
  </footer>
</main>

<style>
  .gallery-grid {
    display: grid;
    gap: 12px;
    margin-top: 8px;
  }
  .gallery-item {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-raised, var(--surface));
    box-shadow: var(--shadow-sm);
    transition: border-color 0.12s ease;
  }
  .gallery-item.decided {
    border-color: var(--accent);
  }
  .gallery-thumb {
    display: flex;
    justify-content: center;
    align-items: center;
    overflow: hidden;
    border-radius: 6px;
    background: var(--surface);
    max-height: 200px;
  }
  .gallery-thumb img,
  .gallery-thumb video {
    max-width: 100%;
    max-height: 200px;
    height: auto;
    object-fit: contain;
  }
  .gallery-label {
    font-weight: 600;
    font-size: 13px;
  }
  .gallery-detail {
    font-size: 12px;
    color: var(--fg-muted, color-mix(in srgb, var(--fg) 62%, var(--bg)));
    white-space: pre-wrap;
  }
  .gallery-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: auto;
  }
  .ga-btn {
    flex: 1 1 auto;
    padding: 4px 8px;
    font-size: 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    cursor: pointer;
  }
  .ga-btn.selected {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 18%, var(--bg));
    font-weight: 600;
  }
  .gallery-comment {
    width: 100%;
    margin-top: 2px;
  }
  .gallery-count {
    flex: 1 1 auto;
    font-size: 12px;
    color: var(--fg-muted, color-mix(in srgb, var(--fg) 55%, var(--bg)));
  }
</style>
