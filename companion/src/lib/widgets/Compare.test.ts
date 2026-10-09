// Review finding D-12: the compare card is one big `role="button"`, and its
// Enter/Space handler called `preventDefault()` for keys from anything inside
// it. Enter on a focused link in a variant's markdown therefore did not open
// the link — it silently picked the variant instead; Space on a video's
// controls did the same instead of play/pause.

import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import Compare from "./Compare.svelte";

afterEach(cleanup);

function renderCompare() {
  return render(Compare, {
    props: {
      spec: {
        kind: "compare",
        variants: [
          { value: "a", content: "Plain" },
          { value: "b", content: "See [the spec](https://example.com/spec)" },
        ],
      },
      onsubmit: vi.fn(),
      oncancel: vi.fn(),
    },
  });
}

describe("Compare — keys inside a card belong to what is focused (D-12)", () => {
  it("lets Enter on a link inside a card through", async () => {
    const { container } = renderCompare();
    const link = container.querySelector<HTMLAnchorElement>(".compare-content a")!;
    expect(link).not.toBeNull();

    const notPrevented = await fireEvent.keyDown(link, { key: "Enter" });

    expect(notPrevented).toBe(true);
    const cards = container.querySelectorAll(".compare-card");
    expect(cards[1].getAttribute("aria-pressed")).toBe("false");
  });

  it("still picks the variant on Enter / Space on the card itself", async () => {
    const { container } = renderCompare();
    const cards = container.querySelectorAll<HTMLElement>(".compare-card");

    await fireEvent.keyDown(cards[1], { key: "Enter" });
    expect(cards[1].getAttribute("aria-pressed")).toBe("true");

    await fireEvent.keyDown(cards[0], { key: " " });
    expect(cards[0].getAttribute("aria-pressed")).toBe("true");
  });
});
