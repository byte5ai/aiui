// Review finding D-01, the widget half: spec values interpolated into a
// `style` attribute. `max_height` / `columns` are never type-checked on the
// way in, so a string like the one below used to become CSS declarations of
// its own — a fixed, full-width layer over the footer, carrying a picture of
// swapped Confirm/Cancel buttons.

import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import "../i18n";
import { boundedInt, boundedNumber, maxHeightStyle } from "./style-values";
import Compare from "./widgets/Compare.svelte";
import Confirm from "./widgets/Confirm.svelte";
import Form from "./widgets/Form.svelte";
import Gallery from "./widgets/Gallery.svelte";

afterEach(cleanup);

const OVERLAY = "1px; position:fixed; left:0; right:0; bottom:0; height:64px; z-index:9; max-height:none";
const IMG = "data:image/png;base64,iVBORw0KGgo=";

/** Every inline style the widget rendered, joined — what the agent got to say. */
function inlineStyles(container: HTMLElement): string {
  return [...container.querySelectorAll<HTMLElement>("[style]")]
    .map((el) => el.getAttribute("style") ?? "")
    .join(" | ");
}

function expectNoInjectedCss(container: HTMLElement) {
  const css = inlineStyles(container);
  expect(css).not.toContain("position");
  expect(css).not.toContain("fixed");
  expect(css).not.toContain("z-index");
  expect(css).not.toContain("none");
}

describe("style-values helpers", () => {
  it("passes numbers and numeric strings through, clamped", () => {
    expect(boundedNumber(240, 0, 1000)).toBe(240);
    expect(boundedNumber("240", 0, 1000)).toBe(240);
    expect(boundedNumber(5000, 0, 1000)).toBe(1000);
    expect(boundedNumber(-5, 0, 1000)).toBe(0);
  });

  it("drops anything that is not a number", () => {
    for (const v of [OVERLAY, "", "  ", null, undefined, NaN, Infinity, {}, [3], true]) {
      expect(boundedNumber(v, 0, 1000)).toBeUndefined();
    }
  });

  it("rounds integers down and keeps them in range", () => {
    expect(boundedInt(3.9, 1, 12)).toBe(3);
    expect(boundedInt(0.5, 1, 12)).toBe(1);
    expect(boundedInt(99, 1, 12)).toBe(12);
    expect(boundedInt("4", 1, 12)).toBe(4);
    expect(boundedInt("3, 1fr); position: fixed", 1, 12)).toBeUndefined();
  });

  it("writes a max-height only for a usable number", () => {
    expect(maxHeightStyle(240)).toBe("max-height: 240px");
    expect(maxHeightStyle(OVERLAY)).toBe("");
    expect(maxHeightStyle(0)).toBe("");
    expect(maxHeightStyle(undefined)).toBe("");
  });
});

describe("widgets interpolate only bounded numbers into style", () => {
  it("confirm image", () => {
    const { container } = render(Confirm, {
      props: {
        spec: { kind: "confirm", title: "Deploy?", image: { src: IMG, max_height: OVERLAY as any } },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });
    expectNoInjectedCss(container);
  });

  it("gallery item and columns", () => {
    const { container } = render(Gallery, {
      props: {
        spec: {
          kind: "gallery",
          columns: "3, 1fr); position: fixed; inset: 0; z-index: 9; (" as any,
          items: [{ value: "a", src: IMG, max_height: OVERLAY as any }],
        },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });
    expectNoInjectedCss(container);
  });

  it("compare pane height and columns", () => {
    const { container } = render(Compare, {
      props: {
        spec: {
          kind: "compare",
          columns: OVERLAY as any,
          variants: [
            { value: "a", content: "A", max_height: OVERLAY as any },
            { value: "b", content: "B" },
          ],
        },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });
    expectNoInjectedCss(container);
    expect(inlineStyles(container)).toContain("repeat(2,");
  });

  it("every form field that styles itself from the spec", () => {
    const { container } = render(Form, {
      props: {
        spec: {
          kind: "form",
          title: "Look",
          fields: [
            { kind: "image", src: IMG, max_height: OVERLAY as any },
            { kind: "annotated_image", name: "spot", src: IMG, max_height: OVERLAY as any },
            {
              kind: "image_grid",
              name: "pick",
              images: [{ value: "a", src: IMG }],
              columns: "3, 1fr); position: fixed; inset: 0; z-index: 9; (" as any,
            },
            { kind: "mermaid", source: "", max_height: OVERLAY as any },
            {
              kind: "wireframe",
              panels: [{ title: "P", col_span: OVERLAY as any, row_span: OVERLAY as any }],
              columns: OVERLAY as any,
              gap: OVERLAY as any,
              max_height: OVERLAY as any,
            },
          ],
        },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });
    expectNoInjectedCss(container);
  });

  it("still applies a real number", () => {
    const { container } = render(Confirm, {
      props: {
        spec: { kind: "confirm", title: "Deploy?", image: { src: IMG, max_height: 240 } },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });
    expect(inlineStyles(container)).toContain("max-height: 240px");
  });
});
