<script lang="ts">
  // Read-only Wireframe renderer for the `wireframe` form-field. Used as
  // an inline-context block (sibling of `markdown` / `image` / `mermaid`)
  // so the agent can show UI-layout mockups — boxed panels in a grid —
  // instead of mauling them into ASCII art in chat.
  //
  // Mermaid covers diagrams (flowcharts, state, sequence, gantt). This
  // field covers the orthogonal class: UI-layouts with fixed-position
  // panels — login screens, dashboard tiles, hardware UIs etc. The agent
  // hands us a structured Box-Spec; we render real CSS-Grid panels
  // instead of asking it to draw monospace boxes-and-pipes.
  //
  // Trade-off for v1: panel content is plain monospace text (multi-line
  // string, newlines preserved). That's enough for mockup-talk (labels,
  // status lines, key-value pairs). Richer content (nested wireframes,
  // images-inside-panel, interactive bits) is deliberately out of scope —
  // the field stays tiny + agent-friendly.

  type Tone = "default" | "muted" | "highlight";

  type Panel = {
    title?: string;
    content?: string;
    col_span?: number;
    row_span?: number;
    tone?: Tone;
  };

  let {
    panels,
    columns = 1,
    gap = 8,
    label,
    max_height,
  }: {
    panels: Panel[];
    columns?: number;
    gap?: number;
    label?: string;
    max_height?: number;
  } = $props();

  // Sanitise grid-cols to a safe positive integer; agents have been
  // observed shipping 0 or negative numbers when the layout-spec was
  // ambiguous. Falling back to 1 (= vertical stack) is the lossless
  // degradation — every panel still renders, just one per row.
  const cols = $derived(Math.max(1, Math.floor(columns) || 1));
  const rowGap = $derived(Math.max(0, Math.floor(gap)));
</script>

<figure
  class="wireframe-block"
  style="--cols: {cols}; --gap: {rowGap}px; {max_height ? `max-height: ${max_height}px` : ''}"
>
  <div class="wireframe-grid">
    {#each panels as panel, i (i)}
      <div
        class="wireframe-panel"
        data-tone={panel.tone ?? "default"}
        style="--col-span: {Math.max(1, Math.floor(panel.col_span ?? 1))}; --row-span: {Math.max(1, Math.floor(panel.row_span ?? 1))}"
      >
        {#if panel.title}
          <div class="wireframe-title">{panel.title}</div>
        {/if}
        {#if panel.content}
          <pre class="wireframe-content">{panel.content}</pre>
        {/if}
      </div>
    {/each}
  </div>
  {#if label}<figcaption>{label}</figcaption>{/if}
</figure>

<style>
  .wireframe-block {
    margin: 0;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    display: flex;
    flex-direction: column;
    gap: 8px;
    overflow: auto;
  }

  .wireframe-grid {
    display: grid;
    grid-template-columns: repeat(var(--cols), minmax(0, 1fr));
    grid-auto-rows: minmax(0, auto);
    gap: var(--gap);
    width: 100%;
  }

  .wireframe-panel {
    grid-column: span var(--col-span, 1);
    grid-row: span var(--row-span, 1);
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-raised, var(--surface));
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0; /* allow content to shrink in narrow columns */
  }

  .wireframe-panel[data-tone="muted"] {
    background: color-mix(in srgb, var(--surface) 90%, var(--muted) 10%);
    color: var(--muted);
    border-color: color-mix(in srgb, var(--border) 70%, transparent);
  }

  .wireframe-panel[data-tone="highlight"] {
    border-color: color-mix(in srgb, var(--accent, var(--success)) 60%, var(--border));
    background: color-mix(in srgb, var(--accent, var(--success)) 6%, var(--surface));
  }

  .wireframe-title {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    border-bottom: 1px solid color-mix(in srgb, var(--border) 80%, transparent);
    padding-bottom: 4px;
  }

  .wireframe-content {
    margin: 0;
    font-family:
      ui-monospace,
      "SF Mono",
      Menlo,
      "Roboto Mono",
      monospace;
    font-size: 11px;
    line-height: 1.45;
    white-space: pre;
    overflow-x: auto;
    color: inherit;
  }

  .wireframe-block figcaption {
    font-size: 11px;
    color: var(--muted);
    text-align: center;
  }
</style>
