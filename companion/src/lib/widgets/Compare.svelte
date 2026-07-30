<script lang="ts">
  import { _ } from "svelte-i18n";
  import { renderMarkdown } from "../markdown";

  type Variant = {
    value: string; // stable id returned as `selected`
    label?: string; // defaults to A / B / C / … by index
    content?: string; // markdown text — draft copy, before/after diff, …
    src?: string; // image or video (data:/http/local→data), same rules as elsewhere
    alt?: string;
    detail?: string; // short caption under the pane (source, score, timestamp, …)
    max_height?: number;
  };
  type Spec = {
    kind: "compare";
    title?: string;
    description?: string;
    header?: string;
    variants: Variant[];
    columns?: number; // override; defaults to variants.length, capped at 4
    syncScroll?: boolean; // lock scroll position across all panes
    submitLabel?: string;
    cancelLabel?: string;
  };

  let { spec, onsubmit, oncancel }: { spec: Spec; onsubmit: (r: any) => void; oncancel: () => void } =
    $props();

  let selected = $state<string | null>(null);

  function defaultLabel(i: number): string {
    return String.fromCharCode(65 + i); // A, B, C, D, …
  }

  function isVideo(src: string | undefined): boolean {
    if (!src) return false;
    if (src.startsWith("data:video/")) return true;
    return /\.(mp4|mov|m4v|webm)(\?|#|$)/i.test(src);
  }

  function pick(e: MouseEvent | KeyboardEvent, value: string) {
    // Two guards so the whole card can be a big, obvious click target
    // (matches the "user clicks a variant" framing from #23) without
    // stepping on normal reading/copying of the compared content:
    //  - a click landing on a link inside markdown content should follow
    //    the link, not also flip the pick;
    //  - a click that ends a text-selection drag (user copying a passage
    //    to quote back) shouldn't be read as a pick either.
    if (e.target instanceof HTMLElement && e.target.closest("a")) return;
    const sel = window.getSelection?.();
    if (sel && sel.toString().length > 0) return;
    selected = value;
  }

  function onKey(e: KeyboardEvent, value: string) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      selected = value;
    }
  }

  const cols = $derived(
    Math.max(1, Math.min(spec.columns && spec.columns > 0 ? spec.columns : spec.variants.length, 4)),
  );

  // Equal-height panes matter more here than per-variant autonomy — a
  // side-by-side compare only reads as "side by side" if the panes line
  // up. So a `max_height` set on ANY variant caps ALL panes, rather than
  // each pane sizing independently.
  const paneMaxHeight = $derived(
    Math.max(360, ...spec.variants.map((v) => v.max_height ?? 0)),
  );

  // --- synchronized scroll --------------------------------------------
  let bodyEls: (HTMLDivElement | null)[] = [];
  let syncing = false;

  function onBodyScroll(i: number) {
    if (!spec.syncScroll || syncing) return;
    const src = bodyEls[i];
    if (!src) return;
    const srcRange = src.scrollHeight - src.clientHeight;
    const ratio = srcRange > 0 ? src.scrollTop / srcRange : 0;
    syncing = true;
    for (let j = 0; j < bodyEls.length; j++) {
      if (j === i) continue;
      const el = bodyEls[j];
      if (!el) continue;
      const range = el.scrollHeight - el.clientHeight;
      el.scrollTop = range > 0 ? ratio * range : 0;
    }
    // Release on the next frame so the synced scrollTop writes above
    // don't re-enter this handler via their own `scroll` events.
    requestAnimationFrame(() => {
      syncing = false;
    });
  }

  function submit() {
    if (selected === null) return;
    onsubmit({ selected });
  }
</script>

<main class="window-shell">
  <div class="window-scroll">
    {#if spec.header}<span class="chip">{spec.header}</span>{/if}
    {#if spec.title}<p class="title">{spec.title}</p>{/if}
    {#if spec.description}<p class="subtitle">{spec.description}</p>{/if}

    <div class="compare-grid" style={`grid-template-columns: repeat(${cols}, minmax(0, 1fr));`}>
      {#each spec.variants as v, i (v.value)}
        <div
          class="compare-card"
          class:selected={selected === v.value}
          role="button"
          tabindex="0"
          aria-pressed={selected === v.value}
          onclick={(e) => pick(e, v.value)}
          onkeydown={(e) => onKey(e, v.value)}
        >
          <div class="compare-head">
            <span class="compare-radio" aria-hidden="true"></span>
            <span class="compare-label">{v.label ?? defaultLabel(i)}</span>
          </div>

          <div
            class="compare-body"
            style={`max-height:${paneMaxHeight}px`}
            bind:this={bodyEls[i]}
            onscroll={() => onBodyScroll(i)}
          >
            {#if v.src}
              <div class="compare-media">
                {#if isVideo(v.src)}
                  <!-- svelte-ignore a11y_media_has_caption -->
                  <video src={v.src} controls preload="metadata"></video>
                {:else}
                  <img src={v.src} alt={v.alt ?? v.label ?? v.value} />
                {/if}
              </div>
            {/if}
            {#if v.content}
              <div class="compare-content">
                <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                {@html renderMarkdown(v.content)}
              </div>
            {/if}
          </div>

          {#if v.detail}<div class="compare-detail">{v.detail}</div>{/if}
        </div>
      {/each}
    </div>
  </div>

  <footer class="window-footer">
    <button onclick={oncancel}>{spec.cancelLabel ?? $_("dialog.cancel")}</button>
    <button class="primary" disabled={selected === null} onclick={submit}
      >{spec.submitLabel ?? $_("dialog.submit")}</button
    >
  </footer>
</main>

<style>
  .compare-grid {
    display: grid;
    gap: 12px;
    align-items: stretch;
  }
  .compare-card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-raised, var(--surface));
    box-shadow: var(--shadow-sm);
    cursor: pointer;
    text-align: left;
    transition: border-color 0.12s ease, background 0.12s ease;
  }
  .compare-card:hover {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
  }
  .compare-card:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .compare-card.selected {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, var(--surface-raised));
  }
  .compare-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .compare-radio {
    width: 14px;
    height: 14px;
    flex: 0 0 14px;
    border-radius: 50%;
    border: 1.5px solid var(--muted);
    background: transparent;
    transition: border-color 0.12s ease, background 0.12s ease;
  }
  .compare-card.selected .compare-radio {
    border-color: var(--accent);
    background: var(--accent);
    box-shadow: inset 0 0 0 2.5px var(--surface-raised, var(--surface));
  }
  .compare-label {
    font-weight: 600;
    font-size: 13px;
  }
  .compare-body {
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    /* Text stays selectable/copyable even though the card itself is a
       click target — see the guard in `pick()`. */
    user-select: text;
    cursor: text;
  }
  .compare-media {
    display: flex;
    justify-content: center;
    align-items: center;
    overflow: hidden;
    border-radius: 6px;
    background: var(--surface);
    cursor: default;
  }
  .compare-media img,
  .compare-media video {
    max-width: 100%;
    height: auto;
    object-fit: contain;
  }
  .compare-content {
    font-size: 13px;
    line-height: 1.5;
  }
  .compare-content :global(p) { margin: 0 0 8px 0; }
  .compare-content :global(p:last-child) { margin-bottom: 0; }
  .compare-content :global(h1),
  .compare-content :global(h2),
  .compare-content :global(h3) { margin: 6px 0 4px; font-size: 14px; }
  .compare-content :global(ul),
  .compare-content :global(ol) { margin: 6px 0; padding-left: 20px; }
  .compare-content :global(li) { margin: 2px 0; }
  .compare-content :global(code) {
    font-family: "SF Mono", Menlo, Consolas, monospace;
    font-size: 12px;
    background: var(--surface);
    padding: 1px 4px;
    border-radius: 4px;
  }
  .compare-content :global(pre) {
    font-family: "SF Mono", Menlo, Consolas, monospace;
    font-size: 12px;
    background: var(--surface);
    padding: 8px 10px;
    border-radius: 6px;
    overflow-x: auto;
  }
  .compare-content :global(pre code) { background: transparent; padding: 0; }
  .compare-content :global(a) { color: var(--accent); }
  .compare-detail {
    font-size: 12px;
    color: var(--fg-muted, color-mix(in srgb, var(--fg) 62%, var(--bg)));
    white-space: pre-wrap;
    cursor: default;
  }
</style>
