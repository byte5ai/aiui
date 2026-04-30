<script lang="ts">
  // Read-only Mermaid renderer for the `mermaid` form-field. Used as
  // an inline-context block (sibling of `markdown` / `image`) so the
  // agent can show flowcharts, sequence diagrams, state machines etc.
  // instead of mauling them into ASCII art in chat.
  //
  // Pipeline:
  //   1. mermaid.render() turns the source DSL into an SVG string
  //   2. DOMPurify sanitises the SVG (script/foreignObject/event-attrs out)
  //   3. {@html} drops it into the DOM
  //
  // mermaid.initialize({ securityLevel: 'strict' }) blocks HTML-in-labels
  // upstream; the DOMPurify pass is the second line of defence.

  import { onMount } from "svelte";
  import mermaid from "mermaid";
  import DOMPurify from "dompurify";

  let { source, label, max_height }: { source: string; label?: string; max_height?: number } = $props();

  let svg = $state("");
  let error = $state<string | null>(null);
  let initialised = false;

  function ensureInit() {
    if (initialised) return;
    mermaid.initialize({
      startOnLoad: false,
      // `default` matches macOS-system-light look out of the box; we
      // don't try to chase the OS dark-mode toggle here — the diagram
      // sits in our themed container, contrast stays readable.
      theme: "default",
      // Strict means: HTML in node labels is rejected (treated as
      // text), markdown links are not rendered as links, no
      // <foreignObject> escape hatches.
      securityLevel: "strict",
    });
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
        svg = DOMPurify.sanitize(rendered, {
          USE_PROFILES: { svg: true, svgFilters: true },
          // Belt-and-braces removals; mermaid + securityLevel:strict
          // shouldn't emit these, but if a future mermaid release ever
          // does, we drop them rather than execute.
          FORBID_TAGS: ["script", "foreignObject"],
          FORBID_ATTR: ["onclick", "onload", "onerror", "onmouseover"],
        });
        error = null;
      })
      .catch((e) => {
        error = String(e?.message ?? e);
        svg = "";
      });
  }

  onMount(() => {
    rerender();
  });

  // Re-render if the source changes (rare in a single dialog, but the
  // agent can in principle re-emit a render with a different spec).
  $effect(() => {
    void source;
    if (initialised) rerender();
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
