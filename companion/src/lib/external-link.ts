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

/**
 * Delegated click handler for a container of rendered markdown.
 *
 * Attach to the wrapper, not to each link — the content is `{@html}` and its
 * anchors have no Svelte lifecycle to hook. Walks up to the nearest `<a>`,
 * so a click on a `<strong>` inside a link still counts.
 *
 * `open_url` already refuses anything that is not http(s) and dispatches via
 * the `open` crate without a shell, so no new command and no new capability
 * entry is needed.
 */
export function handleContentClick(event: MouseEvent): void {
  const target = event.target as Element | null;
  const anchor = target?.closest?.("a");
  if (!anchor) return;
  const href = anchor.getAttribute("href");
  if (!href) return;

  // Always swallow the click: even a href we will not open must not be
  // allowed to navigate the dialog away.
  event.preventDefault();

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
