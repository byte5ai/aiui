// Issue #189 / review finding D-07. A click on a link inside agent content
// must open the user's browser and never navigate the dialog window. Three
// link shapes used to slip past the interceptor: an image-map `<area>` (not
// an `<a>`), and an SVG `<a xlink:href>` (an `<a>`, but without a plain
// `href`, so the handler returned before `preventDefault()`).
//
// The fixtures go through `renderMarkdown`, so they are exactly what the
// sanitiser lets reach the DOM.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const { handleContentClick } = await import("./external-link");
const { renderMarkdown } = await import("./markdown");

let host: HTMLDivElement;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  vi.spyOn(console, "warn").mockImplementation(() => {});
  host = document.createElement("div");
  host.addEventListener("click", handleContentClick);
  document.body.appendChild(host);
});

afterEach(() => host.remove());

/** Click `selector` inside rendered markdown; true when the default ran. */
function click(markdown: string, selector: string): boolean {
  host.innerHTML = renderMarkdown(markdown);
  const el = host.querySelector(selector);
  expect(el, `${selector} survived sanitising`).not.toBeNull();
  return el!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
}

function opened(): string[] {
  return invoke.mock.calls.filter((c) => c[0] === "open_url").map((c) => c[1].url);
}

describe("handleContentClick", () => {
  it("opens a plain link in the browser", () => {
    expect(click("[docs](https://example.com/docs)", "a")).toBe(false);
    expect(opened()).toEqual(["https://example.com/docs"]);
  });

  it("catches an SVG link that only has xlink:href", () => {
    const md =
      '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">' +
      '<a xlink:href="https://example.com/svg"><text>go</text></a></svg>';
    expect(click(md, "text")).toBe(false);
    expect(opened()).toEqual(["https://example.com/svg"]);
  });

  it("catches an image-map area", () => {
    // `renderMarkdown` drops `<map>`/`<area>` now (D-03); the handler still
    // covers them in case a sanitiser change ever lets one through.
    host.innerHTML =
      '<map name="m"><area shape="rect" coords="0,0,10,10" href="https://example.com/area" alt="x"></map>';
    const area = host.querySelector("area")!;
    const ran = area.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    expect(ran).toBe(false);
    expect(opened()).toEqual(["https://example.com/area"]);
  });

  it("swallows a link it will not open", () => {
    expect(click('<a href="/relative">rel</a>', "a")).toBe(false);
    expect(opened()).toEqual([]);
  });

  it("leaves clicks outside links alone", () => {
    expect(click("just **text**", "strong")).toBe(true);
    expect(opened()).toEqual([]);
  });
});
