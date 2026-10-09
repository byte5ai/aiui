<script lang="ts">
  // Read-only Mermaid renderer for the `mermaid` form-field. Used as
  // an inline-context block (sibling of `markdown` / `image`) so the
  // agent can show flowcharts, sequence diagrams, state machines etc.
  // instead of mauling them into ASCII art in chat.
  //
  // Pipeline:
  //   1. mermaid.render() turns the source DSL into an SVG string
  //   2. mermaidImageSrc() sanitises it and encodes it as a data: URL
  //   3. an <img> shows it — the SVG markup never enters this document
  //
  // Issue #189: node labels used to come out empty. Mermaid 11 renders
  // flowchart labels as HTML inside `<foreignObject>`, which the svg
  // sanitiser profile strips. The fix is `htmlLabels: false` at init, so
  // labels stay in `<text>`/`<tspan>`; see ../mermaid-config.ts.
  //
  // Issue #212: a `classDef` turns agent-supplied text into CSS, and inline
  // that CSS reached the dialog — an invisible full-window sheet over the
  // Confirm button. 0.11.0 closed it by stripping every style element and
  // `style=` from the diagram, which also stripped Mermaid's theme: nodes
  // rendered as black boxes with black labels (review finding D-04).
  //
  // Rendering the SVG as an image resolves both. An `<img>` is its own
  // document: its stylesheet cannot reach the dialog, its links are inert,
  // its scripts never run and it loads nothing external. So the theme
  // stays, and a hostile `classDef` can only repaint its own diagram.
  // `mermaid-config.test.ts` holds the component to that path.
  //
  // One window remains where the markup is live in this document: while
  // `mermaid.render()` lays the diagram out, it mounts the SVG (theme
  // stylesheet and inline `classDef` styles included) to measure it. Left to
  // itself it mounts into `document.body`. We hand it `sandbox` instead: off
  // screen, invisible, inert to the pointer, and with layout + paint
  // containment, so even a `position: fixed` node is positioned and clipped
  // inside it rather than over the dialog. Every rule Mermaid emits is
  // scoped to the diagram's own `#id`; the rule-breakout payloads that would
  // escape that scope are refused by Mermaid's parser (see the tests).

  import mermaid from "mermaid";
  import { _ } from "svelte-i18n";
  import { MERMAID_INIT_CONFIG, mermaidImageSrc } from "../mermaid-config";
  import { maxHeightStyle } from "../style-values";

  let { source, label, max_height }: { source: string; label?: string; max_height?: number } = $props();

  /** `data:image/svg+xml;base64,…` of the sanitised diagram, or "". */
  let imgSrc = $state("");
  let error = $state<string | null>(null);
  /** Where Mermaid does its measuring render. See the header comment.
   *  Deliberately not `$state`: the effect below must re-render when
   *  `source` changes, not a second time when this binding lands. */
  let sandbox: HTMLDivElement | undefined;
  let initialised = false;

  function ensureInit() {
    if (initialised) return;
    mermaid.initialize(MERMAID_INIT_CONFIG);
    initialised = true;
  }

  function rerender() {
    ensureInit();
    if (!source) {
      imgSrc = "";
      error = null;
      return;
    }
    const id = `aiui-mermaid-${Math.random().toString(36).slice(2, 10)}`;
    mermaid
      .render(id, source, sandbox)
      .then(({ svg: rendered }) => {
        imgSrc = mermaidImageSrc(rendered);
        error = null;
      })
      .catch((e) => {
        error = String(e?.message ?? e);
        imgSrc = "";
      })
      .finally(() => {
        // Mermaid cleans up its scratch element on most paths, but not all:
        // a `classDef` it refuses while building styles throws past its own
        // cleanup. Only our own element, by id — never another render's.
        sandbox?.querySelector(`#d${id}`)?.remove();
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

<!-- D-01: `max_height` reaches `style` only as a bounded number. -->
<figure class="mermaid-block" style={maxHeightStyle(max_height)}>
  <div class="mermaid-sandbox" bind:this={sandbox} aria-hidden="true"></div>
  {#if error}
    <pre class="mermaid-error">{error}
{@html "<!-- source -->"}{source}</pre>
  {:else if imgSrc}
    <!-- Never `{@html}`: see the header comment. -->
    <img class="mermaid-img" src={imgSrc} alt={$_("dialog.mermaid.alt")} />
  {/if}
  {#if label}<figcaption>{label}</figcaption>{/if}
</figure>

<style>
  .mermaid-block {
    position: relative;
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
  /* Mermaid's measuring render (see the script header). `width: 100%` so a
     diagram that sizes itself to its container (gantt) gets the figure's
     width; everything else keeps it out of sight and out of the dialog. */
  .mermaid-sandbox {
    position: absolute;
    top: 0;
    left: -100000px;
    width: 100%;
    visibility: hidden;
    pointer-events: none;
    overflow: hidden;
    contain: layout paint;
  }
  /* The diagram is drawn in Mermaid's light `default` theme, and an image
     cannot pick up the app's colour tokens. A light plate keeps the theme's
     dark edges and labels legible when the dialog itself is dark. */
  .mermaid-img {
    display: block;
    max-width: 100%;
    height: auto;
    background: #fff;
    border-radius: 4px;
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
