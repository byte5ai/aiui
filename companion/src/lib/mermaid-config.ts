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
};

/**
 * Sanitise a rendered Mermaid SVG before it reaches the DOM.
 *
 * Deliberately unchanged by #189: the sanitiser was never the thing that
 * needed loosening. `<style>` is forbidden alongside `<script>` because
 * Mermaid's `classDef` directive turns caller-supplied text into emitted
 * CSS, and attacker-controlled CSS in a dialog window is UI redressing —
 * these windows are where the user clicks Confirm on destructive actions.
 *
 * #212 measured where that CSS actually lands, and the `<style>` element is
 * not the whole answer: Mermaid emits a `classDef`'s declarations as an
 * inline `style` attribute on the styled node, verbatim and with
 * `!important` appended to every one of them. DOMPurify's svg profile allows
 * `style`, and DOMPurify does not parse the CSS inside it — so a source line
 * like
 *
 *   classDef x position:fixed,top:0,width:100vw,height:100vh,z-index:99999,opacity:0.02
 *
 * reached the DOM intact as a near-invisible overlay covering the whole
 * dialog, including the Confirm button. Hence `style` is forbidden as an
 * attribute too, not only as an element. The cost is nil: the theme's own
 * styling travels in the `<style>` block this sanitiser already drops, so
 * nothing we ship was getting its look from an inline `style` either.
 *
 * Both halves are exercised against the real renderer in
 * `mermaid-config.test.ts` — a Mermaid bump that moves caller-supplied CSS
 * to some third channel is meant to turn that red.
 */
export function sanitizeMermaidSvg(raw: string): string {
  return DOMPurify.sanitize(raw, {
    USE_PROFILES: { svg: true, svgFilters: true },
    FORBID_TAGS: ["script", "style", "foreignObject"],
    FORBID_ATTR: ["style", "onclick", "onload", "onerror", "onmouseover"],
  });
}
