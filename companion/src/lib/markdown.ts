// Shared Markdown → sanitized-HTML renderer, used by every widget that
// renders a `markdown` field or markdown-flavoured content (Form's
// `markdown` field, Compare's per-variant `content`). Extracted from
// Form.svelte (v0.4.10, issue #H-2) so the sanitization allowlist has one
// definition instead of being copy-pasted per widget — a security-relevant
// config like this should not drift between call sites.
//
// Configured once at module scope, used synchronously (no remote includes,
// no async resolvers) so callers stay simple. Output is piped through
// DOMPurify before any `{@html}` use so an MCP caller (potentially a
// compromised remote host reaching us through the SSH-reverse-tunnel)
// cannot inject `<script>` or event handlers.
import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({ gfm: true, breaks: true });

// Issue #189: mark every surviving link as external. The dialog window is
// not allowed to navigate (Rust refuses it), and `external-link.ts`
// intercepts the click to hand the URL to the OS — but if either of those
// ever fails to fire, `target="_blank"` plus `rel="noopener noreferrer"` is
// the difference between "opens somewhere else" and "destroys the dialog".
// Installed once at module scope; DOMPurify hooks are global.
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A" && node.hasAttribute("href")) {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

export function renderMarkdown(src: string): string {
  let raw: string;
  try {
    raw = marked.parse(src, { async: false }) as string;
  } catch {
    raw = `<pre>${src.replace(/[<>&]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;" }[c]!))}</pre>`;
  }
  return DOMPurify.sanitize(raw, {
    // No <script>, no event handlers, no javascript: URLs, no <iframe>.
    // Keep links + basic markup. Defaults are conservative; we explicitly
    // forbid form-related tags to prevent autofill-driven exfiltration.
    FORBID_TAGS: ["script", "iframe", "form", "input", "button"],
    FORBID_ATTR: ["onerror", "onload", "onclick", "onmouseover", "onfocus"],
    // The hook above adds these after the allow-list is applied; naming
    // them here keeps a future DOMPurify version from stripping them.
    ADD_ATTR: ["target", "rel"],
  });
}
