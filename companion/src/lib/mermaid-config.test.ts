// Issue #212. `mermaid` shipped two advisories — "Improper sanitization of
// `classDef` in state diagrams leads to HTML injection" and "Improper
// sanitization of `classDefs` leads to CSS injection" — against the exact
// version range aiui pinned, and `securityLevel: "strict"` is no answer to
// them: it rejects HTML in *node labels*, while `classDef` is a styling
// directive that Mermaid turns into emitted CSS.
//
// That matters more here than on a docs site. An aiui dialog is the window in
// which the user clicks Confirm on a destructive action, and the diagram
// source is agent-supplied — so caller-controlled CSS inside it is UI
// redressing: move, hide or relabel the affirmative button.
//
// These tests render hostile sources through the real Mermaid and the real
// sanitiser, so the verdict is measured rather than argued, and so a Mermaid
// bump that moves caller-supplied CSS to another channel turns them red. The
// second half guards the opposite direction: strict must not mean empty
// (#189).

import { beforeAll, describe, expect, it } from "vitest";
import mermaid from "mermaid";
import { MERMAID_INIT_CONFIG, sanitizeMermaidSvg } from "./mermaid-config";

// jsdom implements no SVG layout, so the measuring calls Mermaid makes while
// laying a diagram out return nothing. Stubbing them is what lets the real
// renderer run headless; none of them influences which elements or attributes
// are emitted, which is all these tests assert on.
function stubSvgMeasurement() {
  const proto = globalThis.SVGElement.prototype as unknown as Record<string, unknown>;
  proto.getBBox = () => ({ x: 0, y: 0, width: 100, height: 20 });
  proto.getScreenCTM = () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0, inverse: () => null });
  proto.getComputedTextLength = () => 100;
}

let seq = 0;

/** The component's pipeline: render the agent's source, then sanitise. */
async function renderAndSanitize(source: string): Promise<string> {
  const { svg } = await mermaid.render(`aiui-test-${(seq += 1)}`, source);
  return sanitizeMermaidSvg(svg);
}

/**
 * Same, but for sources Mermaid may refuse outright. A parse failure is a
 * perfectly good outcome — `rerender()` catches it and shows the error branch,
 * so nothing reaches the DOM — but it must not be the *only* thing standing
 * between the payload and the user, hence the accepted cases are asserted too.
 */
async function renderOrRefuse(source: string): Promise<string> {
  try {
    return await renderAndSanitize(source);
  } catch {
    return "";
  }
}

beforeAll(() => {
  stubSvgMeasurement();
  mermaid.initialize(MERMAID_INIT_CONFIG);
});

describe("caller-supplied CSS cannot reach a dialog", () => {
  // The one that actually got through before #212. Every declaration Mermaid
  // accepts here is written verbatim into an inline `style` attribute, with
  // `!important` appended — a full-viewport, all-but-invisible sheet over the
  // Confirm button, wearing no `<style>` element and no `<script>`.
  it("drops a classDef that overlays the window", async () => {
    const out = await renderAndSanitize(
      [
        "flowchart TD",
        "  A[Start] --> B[End]",
        "  classDef evil position:fixed,top:0,left:0,width:100vw,height:100vh,z-index:99999,opacity:0.02",
        "  class A evil",
      ].join("\n"),
    );

    expect(out).not.toContain("position:fixed");
    expect(out).not.toContain("100vw");
    expect(out).not.toContain("99999");
    // No inline `style` at all, on any element — `\s` so this does not trip
    // over presentation attributes like `font-style=`.
    expect(out).not.toMatch(/\sstyle=/i);
  });

  // The plain colour case, to pin the rule rather than the one payload: no
  // `classDef` declaration whatsoever survives into the DOM, hostile-looking
  // or not.
  it("drops an innocuous-looking classDef too", async () => {
    const out = await renderAndSanitize(
      ["flowchart TD", "  A[Start] --> B[End]", "  classDef c fill:#abcdef,stroke-width:99px", "  class A c"].join("\n"),
    );

    expect(out).not.toContain("#abcdef");
    expect(out).not.toContain("99px");
  });

  // The CSS-injection advisory in the shape it is reported: a `classDef`
  // whose value closes Mermaid's rule and opens the attacker's own. Mermaid
  // 11.17 rejects this at parse time; the assertion is on the outcome, not on
  // which layer produced it, so it keeps holding if a later version accepts
  // the source again and leaves the sanitiser to deal with it.
  it("lets no rule-breakout payload through, in a flowchart", async () => {
    const out = await renderOrRefuse(
      [
        "flowchart TD",
        "  A[Start] --> B[End]",
        "  classDef evil fill:#fff}#confirm-button{opacity:0;position:absolute;left:-9999px",
        "  class A evil",
      ].join("\n"),
    );

    expect(out).not.toMatch(/<style/i);
    expect(out).not.toContain("#confirm-button");
    expect(out).not.toContain("-9999px");
  });

  // ...and the state-diagram half of the advisory pair, which is reported as
  // HTML injection rather than CSS injection.
  it("lets no rule-breakout payload through, in a state diagram", async () => {
    const out = await renderOrRefuse(
      [
        "stateDiagram-v2",
        "  [*] --> Idle",
        "  Idle --> Done",
        "  classDef evil fill:red}</style><style>#aiui-confirm{display:none",
        "  class Idle evil",
      ].join("\n"),
    );

    expect(out).not.toMatch(/<style/i);
    expect(out).not.toContain("#aiui-confirm");
    expect(out).not.toContain("display:none");
  });

  // Not a `classDef`, but the same target: Mermaid emits a `<style>` block for
  // every diagram it renders. The sanitiser forbids the element outright
  // rather than trying to tell our theme's CSS from the caller's, so this
  // holds for a diagram with no hostile input in it at all.
  it("strips the theme <style> block Mermaid always emits", async () => {
    const { svg: raw } = await mermaid.render("aiui-test-theme", "flowchart TD\n  A[Start] --> B[End]");
    expect(raw).toMatch(/<style/i);

    expect(sanitizeMermaidSvg(raw)).not.toMatch(/<style/i);
  });

  it("still strips script and event handlers", () => {
    const out = sanitizeMermaidSvg(
      '<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script>' +
        '<rect onclick="alert(2)" onload="alert(3)" width="10" height="10"></rect></svg>',
    );

    expect(out).not.toContain("<script");
    expect(out).not.toContain("onclick");
    expect(out).not.toContain("onload");
    expect(out).toContain("<rect");
  });
});

describe("the sanitiser does not empty the diagram out", () => {
  // #189: node labels came out blank because Mermaid 11 renders them as HTML
  // inside `<foreignObject>`, which the svg profile strips along with its
  // children. `htmlLabels: false` keeps them in `<text>`/`<tspan>` instead.
  // This is the "Verfassungsorgane" regression in miniature — if a Mermaid
  // bump goes back to HTML labels, the diagram goes blank and this goes red.
  it("keeps flowchart node labels after sanitising", async () => {
    const out = await renderAndSanitize("flowchart TD\n  A[Verfassungsorgane] --> B[Bundestag]");

    expect(out).toContain("Verfassungsorgane");
    expect(out).toContain("Bundestag");
    expect(out).not.toMatch(/<foreignobject/i);
  });

  it("keeps the label even when the source asks for HTML labels back", async () => {
    // Root-level `htmlLabels: false` must win over an init directive in
    // agent-supplied source; otherwise the caller could reopen the
    // `<foreignObject>` path the strict profile exists to close.
    const out = await renderAndSanitize(
      '%%{init: {"flowchart": {"htmlLabels": true}} }%%\nflowchart TD\n  A[Verfassungsorgane] --> B[Bundestag]',
    );

    expect(out).toContain("Verfassungsorgane");
    expect(out).not.toMatch(/<foreignobject/i);
  });

  it("keeps the nodes and edges themselves", async () => {
    const out = await renderAndSanitize("flowchart TD\n  A[Start] --> B[End]");

    expect(out).toMatch(/<svg/i);
    expect(out).toMatch(/<rect/i);
    expect(out).toMatch(/<path/i);
  });
});
