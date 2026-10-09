// Mermaid setup for the read-only `mermaid` form field, extracted so it can
// be unit-tested and so the two halves — how we ask Mermaid to render, and
// how we sanitise what it produces — cannot drift apart silently. Issue #189.

import DOMPurify from "dompurify";

/**
 * How we initialise Mermaid.
 *
 * The load-bearing key is `htmlLabels: false`. Mermaid 11 renders flowchart
 * node labels as HTML inside `<foreignObject>`, and our sanitiser runs with
 * `USE_PROFILES: { svg, svgFilters }` — a profile *replaces* DOMPurify's
 * allow-list, and `foreignobject` is both outside the svg profile and in
 * `DEFAULT_FORBID_CONTENTS`, so the element and everything inside it were
 * stripped. Diagrams rendered as boxes with no text in them.
 *
 * The fix is to not emit HTML labels at all, rather than to widen the
 * sanitiser: admitting `<foreignObject>` would mean admitting `<div>`,
 * `<span>`, `<img>` and `<a href>` inside the SVG, which is exactly the
 * HTML-in-SVG surface the strict profile exists to close — for content an
 * agent supplied.
 *
 * Root-level `htmlLabels`, not `flowchart.htmlLabels`: the scoped key is
 * deprecated in Mermaid 11 and loses to the root one anyway. The root key
 * is also directive-proof against agent-supplied source, which matters
 * here because `source` is untrusted — see the tests.
 *
 * Cost: long labels lose HTML word-wrapping (Mermaid falls back to its own
 * `<tspan>` line-breaking; `<br/>` still works) and FontAwesome `fa:fa-x`
 * substitution stops resolving, because that only runs on the HTML branch.
 * Neither was ever advertised in `docs/skill.md`.
 */
export const MERMAID_INIT_CONFIG = {
  startOnLoad: false,
  // `default` matches the macOS-system-light look out of the box; we don't
  // chase the OS dark-mode toggle here — the diagram sits in our themed
  // container and contrast stays readable.
  theme: "default" as const,
  // Strict: HTML in node labels is rejected (treated as text), markdown
  // links are not rendered as links, no `<foreignObject>` escape hatches.
  securityLevel: "strict" as const,
  htmlLabels: false,
  // Without this, a source Mermaid cannot parse or draw leaves its scratch
  // element — the diagram so far, its `<style>` and any `classDef`-styled
  // node — in `document.body` for good: `render()` throws before its own
  // cleanup runs. MermaidView shows the error itself, so Mermaid's
  // "syntax error" bomb graphic was never wanted anyway.
  suppressErrorRendering: true,
};

/**
 * Sanitise a rendered Mermaid SVG and return it as a standalone XML document.
 *
 * The output is never inserted into the dialog's DOM. It is only ever shown
 * through {@link mermaidImageSrc}, i.e. as an `<img>`, and that is the
 * actual security boundary here:
 *
 * #212 measured that Mermaid's `classDef` turns caller-supplied text into
 * emitted CSS, verbatim and with `!important` appended, both in the theme
 * `<style>` block and as an inline `style` attribute on the styled node. A
 * source line like
 *
 *   classDef x position:fixed,top:0,width:100vw,height:100vh,z-index:99999,opacity:0.02
 *
 * reached the DOM intact as a near-invisible overlay covering the whole
 * dialog, including the Confirm button. The 0.11.0 fix forbade both the
 * `<style>` element and the `style` attribute. That closed the overlay, but
 * the theme's own fills, strokes and label colours travel in exactly that
 * stylesheet, so every node rendered as a black box with black text on it
 * (review finding D-04): legible markup, nothing a user could read.
 *
 * An SVG shown as an image is a separate document. Its stylesheet can only
 * style that document, never the dialog around it; its scripts never run;
 * its links are inert; it cannot load anything external. So the theme
 * `<style>` and inline styles can stay. `classDef` colours work again, and a
 * hostile `classDef` can at most repaint the diagram it came with.
 *
 * DOMPurify still runs, as defence in depth and to keep the markup small:
 * `<script>`, event handlers and `<foreignObject>` (which `htmlLabels: false`
 * should keep from ever being emitted) are dropped.
 *
 * Returns `""` when nothing renderable survives.
 */
export function sanitizeMermaidSvg(raw: string): string {
  const body = DOMPurify.sanitize(raw, {
    USE_PROFILES: { svg: true, svgFilters: true },
    FORBID_TAGS: ["script", "foreignObject"],
    FORBID_ATTR: ["onclick", "onload", "onerror", "onmouseover", "onfocus", "onbegin", "onend"],
    RETURN_DOM: true,
  }) as HTMLElement;
  const svg = body.querySelector("svg");
  if (!svg) return "";
  fixIntrinsicSize(svg);
  // XMLSerializer, not `outerHTML`: an `<img>` parses the payload as XML, and
  // the HTML serialiser writes things like `&nbsp;` that XML rejects, which
  // would turn the whole diagram into a broken-image icon.
  return new XMLSerializer().serializeToString(svg);
}

/**
 * Mermaid sizes its root `<svg>` for inline use: `width="100%"`, a
 * `max-width` in the style attribute, and no `height`. An image needs an
 * intrinsic size instead, or the browser falls back to 300x150 and
 * letterboxes the diagram. Take it from the `viewBox`.
 */
function fixIntrinsicSize(svg: Element): void {
  const vb = (svg.getAttribute("viewBox") ?? "").trim().split(/[\s,]+/).map(Number);
  if (vb.length !== 4 || !vb.every(Number.isFinite)) return;
  const [, , w, h] = vb;
  if (w <= 0 || h <= 0) return;
  svg.setAttribute("width", String(Math.ceil(w)));
  svg.setAttribute("height", String(Math.ceil(h)));
}

/** UTF-8 safe base64. `btoa` alone throws on anything outside Latin-1, and
 *  diagram labels are agent-supplied text in any script. */
function base64Utf8(s: string): string {
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(bin);
}

/**
 * The only way a Mermaid diagram reaches the dialog: as the `src` of an
 * `<img>`. See {@link sanitizeMermaidSvg} for why that, and not the
 * sanitiser, is what keeps diagram CSS out of the dialog.
 *
 * Returns `""` when there is nothing to show.
 */
export function mermaidImageSrc(raw: string): string {
  const xml = sanitizeMermaidSvg(raw);
  return xml ? `data:image/svg+xml;base64,${base64Utf8(xml)}` : "";
}
