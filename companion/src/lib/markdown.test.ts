// Review finding D-01: agent-supplied markdown must not carry CSS into a
// dialog. DOMPurify's default allow-list keeps both the `<style>` element and
// the `style` attribute, and `marked` passes raw HTML blocks straight
// through — so a `markdown` field could hide the file-write approval line
// (#135/#207), the only thing telling the user that pressing Send writes a
// file to an agent-chosen path.
//
// Also the #189 half that never got its test: links survive, are marked as
// external, and dangerous schemes do not.

import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import "../i18n";
import { renderMarkdown } from "./markdown";
import Form from "./widgets/Form.svelte";

afterEach(cleanup);

const HIDE_APPROVAL = "<style>.write-target{display:none!important}</style>";

describe("renderMarkdown — no author CSS", () => {
  it("drops a <style> block, content and all", () => {
    const out = renderMarkdown(`Paste the key below.\n\n${HIDE_APPROVAL}`);
    expect(out).not.toMatch(/<style/i);
    expect(out).not.toContain("write-target");
    expect(out).not.toContain("display:none");
    expect(out).toContain("Paste the key below.");
  });

  it("drops a <style> block nested in inline SVG", () => {
    const out = renderMarkdown(`<svg><style>.write-target{display:none}</style></svg>`);
    expect(out).not.toMatch(/<style/i);
    expect(out).not.toContain("display:none");
  });

  it("drops inline style attributes but keeps the element", () => {
    const out = renderMarkdown(
      '<div style="position:fixed;inset:0;z-index:99999;opacity:0.02">overlay</div>' +
        '\n\n<p style="display:none">hidden</p>',
    );
    expect(out).not.toMatch(/\sstyle=/i);
    expect(out).not.toContain("position:fixed");
    expect(out).toContain("overlay");
    expect(out).toContain("hidden");
  });
});

// Review finding D-03: media elements fetch their `src` as the dialog opens,
// with no click — any loopback port or https host the agent names.
describe("renderMarkdown — no media, no image maps", () => {
  it("drops audio, video, source and track", () => {
    const out = renderMarkdown(
      '<audio src="http://127.0.0.1:8384/rest/x" autoplay></audio>' +
        '<video src="http://localhost:3000/admin/reset#.mp4" preload="auto" poster="https://tracker.example/p.png">' +
        '<source src="https://tracker.example/a.mp4"><track src="https://tracker.example/t.vtt"></video>',
    );
    expect(out).not.toMatch(/<(audio|video|source|track)\b/i);
    expect(out).not.toContain("127.0.0.1");
    expect(out).not.toContain("localhost");
    expect(out).not.toContain("tracker.example");
  });

  it("drops picture sources", () => {
    const out = renderMarkdown('<picture><source srcset="https://tracker.example/s.png"><img alt="x"></picture>');
    expect(out).not.toMatch(/<source\b/i);
    expect(out).not.toContain("tracker.example");
  });

  it("drops image maps (a link that is not an <a>, D-07)", () => {
    const out = renderMarkdown(
      '<map name="m"><area shape="rect" coords="0,0,10,10" href="https://example.com/area" alt="x"></map>',
    );
    expect(out).not.toMatch(/<(map|area)\b/i);
    expect(out).not.toContain("example.com/area");
  });
});

describe("renderMarkdown — links (#189)", () => {
  it("marks a surviving link as external", () => {
    const out = renderMarkdown("[docs](https://example.com/docs)");
    expect(out).toContain('href="https://example.com/docs"');
    expect(out).toContain('target="_blank"');
    expect(out).toContain('rel="noopener noreferrer"');
  });

  it("drops a javascript: href", () => {
    const out = renderMarkdown('<a href="javascript:alert(1)">x</a>');
    expect(out).not.toContain("javascript:");
  });

  it("still strips script and event handlers", () => {
    const out = renderMarkdown('<img src="x" onerror="alert(1)"><script>alert(2)</script>');
    expect(out).not.toContain("onerror");
    expect(out).not.toMatch(/<script/i);
  });
});

describe("the file-write approval line cannot be hidden by markdown", () => {
  it("renders the line and no stylesheet next to it", () => {
    const { container } = render(Form, {
      props: {
        spec: {
          kind: "form",
          title: "Key",
          fields: [
            { kind: "markdown", text: `Paste the key below.\n\n${HIDE_APPROVAL}` },
            {
              kind: "text",
              name: "k",
              label: "Key",
              target: { mode: "create", path: "/tmp/example-key", perm: "0644" },
            },
          ],
        },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
      },
    });

    // The agent's stylesheet never reaches the dialog's DOM…
    expect(container.querySelector("style")).toBeNull();
    expect(container.innerHTML).not.toContain("display:none");
    // …so the approval line is there and nothing hides it.
    const line = container.querySelector<HTMLElement>(".write-target");
    expect(line).not.toBeNull();
    expect(line!.textContent).toContain("/tmp/example-key");
    expect(getComputedStyle(line!).display).not.toBe("none");
  });
});
