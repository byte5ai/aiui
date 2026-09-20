<script lang="ts">
  // Read-only Mermaid renderer for the `mermaid` form-field. Used as
  // an inline-context block (sibling of `markdown` / `image`) so the
  // agent can show flowcharts, sequence diagrams, state machines etc.
  // instead of mauling them into ASCII art in chat.
  //
  // Pipeline:
  //   1. mermaid.render() turns the source DSL into an SVG string
  //   2. sanitizeMermaidSvg() strips script/style/event handlers
  //   3. {@html} drops it into the DOM
  //
  // Issue #189: node labels used to come out empty. Mermaid 11 renders
  // flowchart labels as HTML inside `<foreignObject>`, and a DOMPurify
  // *profile* replaces the allow-list rather than adding to it — so with
  // `USE_PROFILES: { svg, svgFilters }` the element was never allowed,
  // and `foreignobject` is additionally in `DEFAULT_FORBID_CONTENTS`, so
  // its children went too. Dropping it from `FORBID_TAGS` in v0.4.38 was
  // therefore a no-op: nothing it forbade was reachable anyway, and the
  // "Verfassungsorgane" regression it was meant to fix stayed broken.
  //
  // The real fix is `htmlLabels: false` at init, so Mermaid keeps labels
  // in `<text>`/`<tspan>` — which the svg profile does allow. The
  // sanitiser stays strict; see ../mermaid-config.ts for why widening it
  // would be the wrong trade.

  import mermaid from "mermaid";
  import { MERMAID_INIT_CONFIG, sanitizeMermaidSvg } from "../mermaid-config";

  let { source, label, max_height }: { source: string; label?: string; max_height?: number } = $props();

  let svg = $state("");
  let error = $state<string | null>(null);
  let initialised = false;

  function ensureInit() {
    if (initialised) return;
    mermaid.initialize(MERMAID_INIT_CONFIG);
    initialised = true;
  }

  function rerender() {
    ensureInit();
    if (!source) {
      svg = "";
      error = null;
      return;
    }
    const id = `aiui-mermaid-${Math.random().toString(36).slice(2, 10)}`;
    mermaid
      .render(id, source)
      .then(({ svg: rendered }) => {
        svg = sanitizeMermaidSvg(rendered);
        error = null;
      })
      .catch((e) => {
        error = String(e?.message ?? e);
        svg = "";
      });
  }

  // One owner for both the first render and any later source change.
  // `onMount` + an `initialised`-gated effect used to mean the first
  // render happened twice on mount in some orderings, and never at all in
  // others — the effect's guard reads a flag `rerender` itself sets.
  $effect(() => {
    void source;
    rerender();
  });
</script>

<figure class="mermaid-block" style={max_height ? `max-height: ${max_height}px` : ""}>
  {#if error}
    <pre class="mermaid-error">{error}
{@html "<!-- source -->"}{source}</pre>
  {:else if svg}
    <!-- {@html svg} — sanitised by DOMPurify above before reaching the DOM -->
    {@html svg}
  {/if}
  {#if label}<figcaption>{label}</figcaption>{/if}
</figure>

<style>
  .mermaid-block {
    margin: 0;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    overflow: auto;
  }
  .mermaid-block :global(svg) {
    max-width: 100%;
    height: auto;
  }
  .mermaid-block figcaption {
    font-size: 11px;
    color: var(--muted);
    text-align: center;
  }
  .mermaid-error {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 8%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--danger) 30%, var(--border));
    padding: 8px 10px;
    border-radius: 6px;
    font-size: 11px;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 160px;
    overflow: auto;
    align-self: stretch;
  }
</style>
