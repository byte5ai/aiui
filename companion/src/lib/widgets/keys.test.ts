// Review findings D-05 / A-02. Every collection a widget renders came through
// a `{#each … (item.value)}` keyed by an agent-supplied value. Svelte throws
// `each_key_duplicate` on a repeated key — in production builds too — and the
// throw tears down the whole mount: an empty, always-on-top window with no
// buttons, and the agent blocked until the user closes it or the TTL ends.
// Two entries without a `value` share the key `undefined`.
//
// Rust's validate_spec refuses such specs; these tests hold the backstop:
// the window renders, and every entry the agent sent is on screen.

import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import Compare from "./Compare.svelte";
import Form from "./Form.svelte";
import Gallery from "./Gallery.svelte";

afterEach(cleanup);

const IMG = "data:image/png;base64,iVBORw0KGgo=";

function renderForm(fields: unknown[], extra: Record<string, unknown> = {}) {
  return render(Form, {
    props: {
      spec: { kind: "form", title: "Pick", fields: fields as any, ...extra },
      onsubmit: vi.fn(),
      oncancel: vi.fn(),
    },
  });
}

function expectButtons() {
  // The footer is the first thing a blanked window loses.
  expect(screen.getByRole("button", { name: "Send" })).toBeTruthy();
}

describe("form collections with a missing or repeated value", () => {
  it("list items without a value", () => {
    renderForm([{ kind: "list", name: "rank", sortable: true, items: [{ label: "Alpha" }, { label: "Beta" }] }]);
    expect(screen.getByText("Alpha")).toBeTruthy();
    expect(screen.getByText("Beta")).toBeTruthy();
    expectButtons();
  });

  it("list items sharing a value", () => {
    renderForm([
      {
        kind: "list",
        name: "rank",
        selectable: true,
        items: [
          { label: "Alpha", value: "x" },
          { label: "Beta", value: "x" },
          { label: "Gamma", value: "y" },
        ],
      },
    ]);
    expect(screen.getByText("Alpha")).toBeTruthy();
    expect(screen.getByText("Gamma")).toBeTruthy();
    expectButtons();
  });

  it("table rows sharing or missing a value — every row still shown once", () => {
    const { container } = renderForm([
      {
        kind: "table",
        name: "rows",
        columns: [{ key: "n", label: "Name" }],
        rows: [
          { value: "a", values: { n: "one" } },
          { value: "a", values: { n: "two" } },
          { values: { n: "three" } },
          { values: { n: "four" } },
        ],
      },
    ]);
    const cells = [...container.querySelectorAll("tbody td")].map((td) => td.textContent);
    expect(cells).toEqual(["one", "two", "three", "four"]);
    expectButtons();
  });

  it("image_grid images sharing a value", () => {
    const { container } = renderForm([
      {
        kind: "image_grid",
        name: "pick",
        images: [
          { value: "a", src: IMG, label: "first" },
          { value: "a", src: IMG, label: "second" },
        ],
      },
    ]);
    expect(container.querySelectorAll(".image-cell")).toHaveLength(2);
    expectButtons();
  });

  it("tree nodes sharing a value", () => {
    renderForm([
      {
        kind: "tree",
        name: "t",
        items: [
          { label: "Root A", value: "r", children: [{ label: "Kid 1", value: "k" }, { label: "Kid 2", value: "k" }] },
          { label: "Root B", value: "r" },
        ],
      },
    ]);
    expect(screen.getByText("Kid 2")).toBeTruthy();
    expect(screen.getByText("Root B")).toBeTruthy();
    expectButtons();
  });

  it("tabs sharing a label", () => {
    renderForm([], {
      fields: undefined,
      tabs: [
        { label: "Same", fields: [{ kind: "text", name: "a", label: "A" }] },
        { label: "Same", fields: [{ kind: "text", name: "b", label: "B" }] },
      ],
    });
    expect(screen.getAllByRole("tab")).toHaveLength(2);
    expectButtons();
  });
});

describe("gallery and compare with a missing or repeated value", () => {
  it("gallery actions without a value", () => {
    const { container } = render(Gallery, {
      props: {
        spec: {
          kind: "gallery",
          items: [{ value: "one", src: IMG }],
          actions: [{ label: "Ship" }, { label: "Hold" }] as any,
        },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });
    expect(container.querySelectorAll(".ga-btn")).toHaveLength(2);
    expectButtons();
  });

  it("gallery actions and items sharing a value", () => {
    const { container } = render(Gallery, {
      props: {
        spec: {
          kind: "gallery",
          items: [
            { value: "same", src: IMG, label: "first" },
            { value: "same", src: IMG, label: "second" },
          ],
          actions: [
            { label: "Ok", value: "ok" },
            { label: "Also ok", value: "ok" },
          ],
        },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });
    expect(container.querySelectorAll(".gallery-item")).toHaveLength(2);
    expectButtons();
  });

  it("compare variants sharing a value", () => {
    const { container } = render(Compare, {
      props: {
        spec: {
          kind: "compare",
          variants: [
            { value: "v", content: "first" },
            { value: "v", content: "second" },
          ],
        },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });
    expect(container.querySelectorAll(".compare-card")).toHaveLength(2);
    expectButtons();
  });
});
