// Issue #212 / review finding D-04. A Mermaid diagram is agent-supplied, and
// `classDef` turns agent-supplied text into emitted CSS. An aiui dialog is the
// window in which the user clicks Confirm on a destructive action, so that
// CSS must never style the dialog: that would be UI redressing.
//
// 0.11.0 kept it out by stripping every `<style>` and `style=` from the
// diagram, which also stripped Mermaid's theme: nodes rendered as black boxes
// with black labels (D-04). The diagram now reaches the dialog only as an
// `<img>` whose `src` is the sanitised SVG. An image is its own document; its
// CSS cannot reach the page around it. So the theme stays, and the guarantee
// moves from "the CSS was removed" to "the markup never enters the DOM".
//
// These tests drive the real Mermaid and the real component, so the verdict
// is measured rather than argued. The first block holds the boundary; the
// second holds the opposite direction: safe must not mean unreadable (#189,
// D-04).

import { cleanup, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import mermaid from "mermaid";
import "../i18n";
import { MERMAID_INIT_CONFIG, mermaidImageSrc, sanitizeMermaidSvg } from "./mermaid-config";
import MermaidView from "./widgets/MermaidView.svelte";

// jsdom implements no SVG layout, so the measuring calls Mermaid makes while
// laying a diagram out return nothing. Stubbing them is what lets the real
// renderer run headless; none of them influences which elements, attributes
// or style rules are emitted, which is all these tests assert on.
function stubSvgMeasurement() {
  const proto = globalThis.SVGElement.prototype as unknown as Record<string, unknown>;
  proto.getBBox = () => ({ x: 0, y: 0, width: 100, height: 20 });
  proto.getScreenCTM = () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0, inverse: () => null });
  proto.getComputedTextLength = () => 100;
}

const PREFIX = "data:image/svg+xml;base64,";

/** The SVG document an `<img src>` produced by `mermaidImageSrc` shows. */
function decodeImage(src: string): string {
  expect(src.startsWith(PREFIX)).toBe(true);
  const bin = atob(src.slice(PREFIX.length));
  return new TextDecoder().decode(Uint8Array.from(bin, (c) => c.charCodeAt(0)));
}

let seq = 0;

/** The component's pipeline, minus the component: render, sanitise, encode. */
async function renderToImage(source: string): Promise<string> {
  const { svg } = await mermaid.render(`aiui-test-${(seq += 1)}`, source);
  return mermaidImageSrc(svg);
}

/** Mount the real widget and wait for it to settle on an image or an error. */
async function mountDiagram(source: string) {
  const view = render(MermaidView, { props: { source } });
  await waitFor(() => {
    expect(view.container.querySelector("img.mermaid-img, .mermaid-error")).not.toBeNull();
  });
  return view;
}

/** Every piece of CSS the dialog's own document carries, anywhere in it. */
function hostDocumentCss(): string {
  const sheets = [...document.querySelectorAll("style")].map((s) => s.textContent ?? "");
  const inline = [...document.querySelectorAll("[style]")].map((e) => e.getAttribute("style") ?? "");
  return [...sheets, ...inline].join("\n");
}

function expectNoDiagramMarkupInDialog(container: HTMLElement) {
  expect(container.querySelectorAll("svg, style")).toHaveLength(0);
  // Nor anywhere else: no scratch element left in <body> by Mermaid.
  expect(document.querySelectorAll('[id^="daiui-"], svg[id^="aiui-"]')).toHaveLength(0);
}

/**
 * The value of `prop` an SVG renderer would resolve for `el` from the
 * diagram's own stylesheet: last matching rule wins, `!important` beats
 * normal, otherwise inherited from the nearest ancestor that sets it. jsdom
 * has no SVG cascade of its own (`getComputedStyle(rect).fill` is always
 * black), so this does the part these tests need. Specificity is ignored;
 * Mermaid's theme rules are all `#id .class element`.
 */
function cascaded(el: Element, prop: string, sheet: CSSStyleSheet): string | null {
  for (let node: Element | null = el; node; node = node.parentElement) {
    let value: string | null = null;
    let important = false;
    for (const rule of [...sheet.cssRules]) {
      if (!("selectorText" in rule)) continue;
      const r = rule as CSSStyleRule;
      let hit = false;
      try {
        hit = node.matches(r.selectorText);
      } catch {
        continue;
      }
      if (!hit) continue;
      const v = r.style.getPropertyValue(prop);
      if (!v) continue;
      const imp = r.style.getPropertyPriority(prop) === "important";
      if (imp || !important) {
        value = v;
        important = imp;
      }
    }
    if (value) return value;
    const attr = node.getAttribute(prop);
    if (attr) return attr;
  }
  return null;
}

function themeOf(xml: string): { doc: Document; sheet: CSSStyleSheet } {
  const doc = new DOMParser().parseFromString(xml, "image/svg+xml");
  const sheet = new CSSStyleSheet();
  sheet.replaceSync([...doc.querySelectorAll("style")].map((s) => s.textContent ?? "").join("\n"));
  return { doc, sheet };
}

const BLACK = /^(black|#000(000)?|rgb\(0,\s*0,\s*0\))$/i;

beforeAll(() => {
  stubSvgMeasurement();
  mermaid.initialize(MERMAID_INIT_CONFIG);
});

afterEach(cleanup);

describe("caller-supplied CSS cannot reach a dialog", () => {
  // The one that got through before #212: Mermaid writes a `classDef`'s
  // declarations verbatim into an inline `style` on the styled node, with
  // `!important` appended.
  it("keeps a classDef overlay inside the image", async () => {
    const { container } = await mountDiagram(
      [
        "flowchart TD",
        "  A[Start] --> B[End]",
        "  classDef evil position:fixed,top:0,left:0,width:100vw,height:100vh,z-index:99999,opacity:0.02",
        "  class A evil",
      ].join("\n"),
    );

    const img = container.querySelector<HTMLImageElement>("img.mermaid-img");
    expect(img).not.toBeNull();
    expect(img!.getAttribute("src")!.startsWith(PREFIX)).toBe(true);
    expectNoDiagramMarkupInDialog(container);
    const css = hostDocumentCss();
    expect(css).not.toContain("99999");
    expect(css).not.toContain("100vw");
    expect(css).not.toContain("position:fixed");
  });

  // The CSS-injection advisory in the shape it is reported: a `classDef`
  // whose value closes Mermaid's rule and opens the attacker's own. Mermaid
  // 11.17 refuses it at parse time; the assertion is on the outcome, not on
  // which layer produced it.
  it("lets no rule-breakout payload through, in a flowchart", async () => {
    const { container } = await mountDiagram(
      [
        "flowchart TD",
        "  A[Start] --> B[End]",
        "  classDef evil fill:#fff}#confirm-button{opacity:0;position:absolute;left:-9999px",
        "  class A evil",
      ].join("\n"),
    );

    expectNoDiagramMarkupInDialog(container);
    const css = hostDocumentCss();
    expect(css).not.toContain("#confirm-button");
    expect(css).not.toContain("-9999px");
  });

  // ...and the state-diagram half of the advisory pair.
  it("lets no rule-breakout payload through, in a state diagram", async () => {
    const { container } = await mountDiagram(
      [
        "stateDiagram-v2",
        "  [*] --> Idle",
        "  Idle --> Done",
        "  classDef evil fill:red}</style><style>#aiui-confirm{display:none",
        "  class Idle evil",
      ].join("\n"),
    );

    expectNoDiagramMarkupInDialog(container);
    const css = hostDocumentCss();
    expect(css).not.toContain("#aiui-confirm");
    expect(css).not.toContain("display:none");
  });

  // Mermaid's own error path used to leave its scratch element — the
  // diagram so far, its `<style>` and any styled node — in `<body>`.
  it("leaves nothing behind when Mermaid cannot parse the source", async () => {
    const { container } = await mountDiagram("flowchart TD\n  A[Start] -->");

    expect(container.querySelector(".mermaid-error")).not.toBeNull();
    expectNoDiagramMarkupInDialog(container);
  });

  it("measures inside its own sandbox, once per source", async () => {
    const spy = vi.spyOn(mermaid, "render");
    const { container } = await mountDiagram("flowchart TD\n  A[Start] --> B[End]");

    expect(spy).toHaveBeenCalledTimes(1);
    const sandbox = spy.mock.calls[0][2] as unknown as Element;
    expect(sandbox).toBe(container.querySelector(".mermaid-sandbox"));
    expect(sandbox.childElementCount).toBe(0);
  });

  it("still strips script, event handlers and foreignObject from the image", () => {
    const xml = sanitizeMermaidSvg(
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><script>alert(1)</script>' +
        '<rect onclick="alert(2)" onload="alert(3)" width="10" height="10"></rect>' +
        "<foreignObject><div>html</div></foreignObject></svg>",
    );

    expect(xml).not.toContain("<script");
    expect(xml).not.toContain("onclick");
    expect(xml).not.toContain("onload");
    expect(xml).not.toMatch(/foreignobject/i);
    expect(xml).toContain("<rect");
  });
});

describe("the diagram is readable (#189, D-04)", () => {
  // D-04: with the theme stylesheet stripped, SVG's initial `fill: black`
  // applied to boxes and labels alike — black on black. The label string was
  // in the markup the whole time, which is all the old test checked.
  it("draws node boxes in the theme's fill and labels in a different one", async () => {
    const xml = decodeImage(await renderToImage("flowchart TD\n  A[Start] --> B[End]"));
    const { doc, sheet } = themeOf(xml);

    const rect = doc.querySelector(".node rect");
    const label = doc.querySelector(".node text");
    expect(rect).not.toBeNull();
    expect(label?.textContent).toContain("Start");

    const boxFill = cascaded(rect!, "fill", sheet);
    const labelFill = cascaded(label!, "fill", sheet);
    expect(boxFill, "node box has no fill: it renders black").not.toBeNull();
    expect(boxFill).not.toMatch(BLACK);
    expect(boxFill).not.toBe("none");
    expect(labelFill, "label has no fill: it renders black").not.toBeNull();
    expect(labelFill).not.toBe(boxFill);
  });

  it("gives the image an intrinsic size instead of width=100%", async () => {
    const xml = decodeImage(await renderToImage("flowchart TD\n  A[Start] --> B[End]"));
    const root = new DOMParser().parseFromString(xml, "image/svg+xml").documentElement;

    expect(root.getAttribute("width")).toMatch(/^\d+$/);
    expect(root.getAttribute("height")).toMatch(/^\d+$/);
  });

  // #189: node labels came out blank because Mermaid 11 renders them as HTML
  // inside `<foreignObject>`. `htmlLabels: false` keeps them in `<text>`.
  it("keeps flowchart node labels", async () => {
    const xml = decodeImage(await renderToImage("flowchart TD\n  A[Verfassungsorgane] --> B[Bundestag]"));

    expect(xml).toContain("Verfassungsorgane");
    expect(xml).toContain("Bundestag");
    expect(xml).not.toMatch(/<foreignobject/i);
  });

  it("keeps the label even when the source asks for HTML labels back", async () => {
    // Root-level `htmlLabels: false` must win over an init directive in
    // agent-supplied source.
    const xml = decodeImage(
      await renderToImage(
        '%%{init: {"flowchart": {"htmlLabels": true}} }%%\nflowchart TD\n  A[Verfassungsorgane] --> B[Bundestag]',
      ),
    );

    expect(xml).toContain("Verfassungsorgane");
    expect(xml).not.toMatch(/<foreignobject/i);
  });

  it("survives labels outside Latin-1", async () => {
    // `btoa` alone throws on these; the whole diagram would fail to show.
    const xml = decodeImage(await renderToImage("flowchart TD\n  A[Größe 日本語 🚀] --> B[Übermorgen]"));

    // Mermaid puts each word in a `<tspan>` of its own.
    for (const word of ["Größe", "日本語", "🚀", "Übermorgen"]) expect(xml).toContain(word);
  });

  it("keeps the nodes and edges themselves", async () => {
    const xml = decodeImage(await renderToImage("flowchart TD\n  A[Start] --> B[End]"));

    expect(xml).toMatch(/<svg/i);
    expect(xml).toMatch(/<rect/i);
    expect(xml).toMatch(/<path/i);
  });
});
