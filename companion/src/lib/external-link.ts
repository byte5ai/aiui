// Links inside agent-supplied content open in the user's real browser, never
// in the dialog window. Issue #189.
//
// A `markdown` or `compare` field renders text the agent wrote, and a plain
// `[text](https://…)` link survives sanitisation — as it should, the user is
// meant to be able to follow it. But a click used to navigate the dialog
// window itself: the Svelte app gone, the dialog's `/render` left hanging
// until its TTL, the user staring at a web page in a frameless window with
// no back button.
//
// Rust refuses the navigation outright (`is_allowed_app_navigation`), which
// is the authoritative half. This is the half that makes the link still
// *work*: intercept the click and hand the URL to the OS.

import { invoke } from "@tauri-apps/api/core";

const XLINK_NS = "http://www.w3.org/1999/xlink";

/**
 * The link target of an `<a>` or `<area>`, in every form the sanitiser lets
 * through: the HTML `href`, or an SVG `<a>`'s `href` / `xlink:href`.
 */
function linkTarget(anchor: Element): string | null {
  return (
    anchor.getAttribute("href") ??
    anchor.getAttributeNS(XLINK_NS, "href") ??
    anchor.getAttribute("xlink:href")
  );
}

/**
 * Delegated click handler for a container of rendered markdown.
 *
 * Attach to the wrapper, not to each link — the content is `{@html}` and its
 * anchors have no Svelte lifecycle to hook. Walks up to the nearest `<a>`,
 * so a click on a `<strong>` inside a link still counts.
 *
 * Review finding D-07: three link shapes used to slip past this and
 * navigate the window. An `<area>` of an image map is not an `<a>`; an SVG
 * `<a xlink:href>` is one, but has no plain `href`, and the handler returned
 * before `preventDefault()`. Both are covered now, and the click is swallowed
 * as soon as any link element is found — whether or not it has a target we
 * would open.
 *
 * `open_url` already refuses anything that is not http(s) and dispatches via
 * the `open` crate without a shell, so no new command and no new capability
 * entry is needed.
 */
export function handleContentClick(event: MouseEvent): void {
  const target = event.target as Element | null;
  const anchor = target?.closest?.("a, area");
  if (!anchor) return;

  // Always swallow the click: even a link we will not open must not be
  // allowed to navigate the dialog away.
  event.preventDefault();

  const href = linkTarget(anchor);
  if (!href) return;

  if (!/^https?:\/\//i.test(href)) {
    // Relative links, `javascript:`, `file:` … nothing to open, and
    // nothing that should reach the OS. Sanitisation already drops the
    // dangerous schemes; this is the belt.
    console.warn(`[aiui] refusing to open non-http(s) link: ${href}`);
    return;
  }

  void invoke("open_url", { url: href }).catch((e) => {
    console.error(`[aiui] open_url failed for ${href}: ${e}`);
  });
}
