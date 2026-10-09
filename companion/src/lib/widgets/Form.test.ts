// #206: the submit path. A required field on a tab the user is not looking at
// used to produce a greyed-out button and nothing else — no message, no
// highlight, no tab jump — while a `destructive` button stayed clickable and
// swallowed the click. These tests hold the visible behaviour in place.

import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import Form from "./Form.svelte";
import type { Action, Tab } from "./form-values";

afterEach(cleanup);

const tabs: Tab[] = [
  { label: "One", fields: [{ kind: "text", name: "a", label: "A" }] },
  { label: "Two", fields: [{ kind: "text", name: "b", label: "B" }] },
  { label: "Three", fields: [{ kind: "text", name: "c", label: "C", required: true }] },
];

function renderForm(overrides: { actions?: Action[] } = {}) {
  const onsubmit = vi.fn();
  const oncancel = vi.fn();
  const { container } = render(Form, {
    props: {
      spec: { kind: "form", title: "Deploy", tabs, ...overrides },
      onsubmit,
      oncancel,
    },
  });
  return { container, onsubmit, oncancel };
}

const tabButtons = () => screen.getAllByRole("tab");

describe("Form — validation is visible", () => {
  it("surfaces an invalid field on another tab instead of a dead button", async () => {
    const { container, onsubmit } = renderForm();

    const submit = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    // A disabled button fires no click — that is what made the tab jump and
    // every other hint unreachable.
    expect(submit.disabled).toBe(false);
    expect(tabButtons()[0].getAttribute("aria-selected")).toBe("true");
    expect(screen.queryByRole("alert")).toBeNull();

    await fireEvent.click(submit);

    expect(onsubmit).not.toHaveBeenCalled();
    expect(tabButtons()[2].getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("alert").textContent).toContain("Three");
    expect(container.querySelectorAll('[aria-invalid="true"]')).toHaveLength(1);
  });

  it("clears the complaint once the field is filled in", async () => {
    const { container, onsubmit } = renderForm();

    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const input = container.querySelector('input[aria-invalid="true"]') as HTMLInputElement;
    expect(input).not.toBeNull();

    await fireEvent.input(input, { target: { value: "because" } });
    expect(screen.queryByRole("alert")).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onsubmit).toHaveBeenCalledTimes(1);
    expect(onsubmit.mock.calls[0][0]).toEqual({
      action: null,
      values: { a: "", b: "", c: "because" },
    });
  });

  it("does not let a destructive button silently no-op", async () => {
    const { onsubmit } = renderForm({
      actions: [
        { label: "Cancel", value: "__cancel__", skip_validation: true },
        { label: "Delete", value: "delete", destructive: true },
      ],
    });

    await fireEvent.click(screen.getByRole("button", { name: "Delete" }));

    expect(onsubmit).not.toHaveBeenCalled();
    expect(screen.getByRole("alert").textContent).toContain("Three");
    expect(tabButtons()[2].getAttribute("aria-selected")).toBe("true");
  });

  it("lets a skip_validation action through untouched", async () => {
    const { onsubmit } = renderForm({
      actions: [
        { label: "Later", value: "defer", skip_validation: true },
        { label: "Send", value: "__submit__", primary: true },
      ],
    });

    await fireEvent.click(screen.getByRole("button", { name: "Later" }));

    expect(screen.queryByRole("alert")).toBeNull();
    expect(onsubmit).toHaveBeenCalledWith({
      action: "defer",
      values: { a: "", b: "", c: "" },
    });
  });
});

describe("Form — normalised defaults reach the agent", () => {
  it("submits what the widgets actually show", async () => {
    const onsubmit = vi.fn();
    render(Form, {
      props: {
        spec: {
          kind: "form",
          title: "Settings",
          fields: [
            {
              kind: "select",
              name: "scope",
              label: "Scope",
              options: [
                { label: "Repo", value: "repo" },
                { label: "Org", value: "org" },
              ],
            },
            { kind: "date", name: "day", label: "Day", default: "2026-06-01T00:00:00Z" },
            { kind: "color", name: "tint", label: "Tint", default: "red" },
            { kind: "slider", name: "pct", label: "Percent", min: 0, max: 100, default: 200 },
          ],
        },
        onsubmit,
        oncancel: vi.fn(),
      },
    });

    await fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(onsubmit).toHaveBeenCalledWith({
      action: null,
      values: { scope: "repo", day: "2026-06-01", tint: "#000000", pct: 100 },
    });
  });

  // D-09, D-13, D-14 end to end: what the agent receives for defaults the
  // user could not see, a zone-less datetime, an untouched number and a
  // slider without bounds.
  it("returns only what was on screen, with a datetime's zone", async () => {
    const onsubmit = vi.fn();
    render(Form, {
      props: {
        spec: {
          kind: "form",
          title: "Schedule",
          fields: [
            {
              kind: "select",
              name: "env",
              label: "Env",
              options: [
                { label: "Production", value: "production" },
                { label: "Staging", value: "staging" },
              ],
              default: "prod",
            },
            { kind: "datetime", name: "at", label: "At", default: "2026-06-01T09:30" },
            { kind: "number", name: "replicas", label: "Replicas" },
            { kind: "slider", name: "pct", label: "Percent" } as any,
            {
              kind: "table",
              name: "rows",
              columns: [{ key: "n", label: "N" }],
              rows: [{ value: "r1", values: { n: "one" } }],
              required: true,
              default_selected: ["row-9"],
            },
          ],
        },
        onsubmit,
        oncancel: vi.fn(),
      },
    });

    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    // The phantom pre-selection does not satisfy `required` any more.
    expect(onsubmit).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByText("one"));
    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onsubmit).toHaveBeenCalledTimes(1);
    const values = onsubmit.mock.calls[0][0].values;
    expect(values.env).toBe("production");
    expect(values.at).toMatch(/^2026-06-01T09:30[+-]\d{2}:\d{2}$/);
    expect(new Date(values.at).getTime()).toBe(new Date(2026, 5, 1, 9, 30).getTime());
    expect(values.replicas).toBeNull();
    expect(values.pct).toBe(0);
    expect(values.rows.selected).toEqual(["r1"]);
  });
});

// Review finding C-02: only the active tab is rendered, and the `target`
// approval line lives next to its field. Submit serialises every tab, and
// the writer writes every `target` of the stored spec — so a target field on
// a tab the user never opened was written without its line ever on screen.
describe("Form — a file write is shown before it happens (C-02)", () => {
  const targetTabs: Tab[] = [
    { label: "Question", fields: [{ kind: "text", name: "why", label: "Why" }] },
    {
      label: "Credentials",
      fields: [
        {
          kind: "text",
          name: "token",
          label: "Token",
          default: "agent-chosen",
          target: { mode: "create", path: "/tmp/example-token", perm: "0644" },
        },
      ],
    },
  ];

  function renderTargets(actions?: Action[]) {
    const onsubmit = vi.fn();
    const { container } = render(Form, {
      props: {
        spec: { kind: "form", title: "Setup", tabs: targetTabs, ...(actions ? { actions } : {}) },
        onsubmit,
        oncancel: vi.fn(),
      },
    });
    return { container, onsubmit };
  }

  it("holds the submit and shows the unseen tab that writes", async () => {
    const { container, onsubmit } = renderTargets();
    expect(container.querySelector(".write-target")).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(onsubmit).not.toHaveBeenCalled();
    expect(tabButtons()[1].getAttribute("aria-selected")).toBe("true");
    expect(container.querySelector(".write-target")?.textContent).toContain("/tmp/example-token");
    expect(screen.getByRole("alert").textContent).toContain("Credentials");

    // Seen now: the second press is the approval.
    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onsubmit).toHaveBeenCalledTimes(1);
  });

  it("submits at once when the user already opened that tab", async () => {
    const { onsubmit } = renderTargets();
    await fireEvent.click(tabButtons()[1]);
    await fireEvent.click(tabButtons()[0]);
    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onsubmit).toHaveBeenCalledTimes(1);
  });

  it("does not hold an action that writes nothing", async () => {
    const { onsubmit } = renderTargets([
      { label: "Later", value: "defer", skip_validation: true },
      { label: "Send", value: "__submit__", primary: true },
    ]);
    await fireEvent.click(screen.getByRole("button", { name: "Later" }));
    expect(onsubmit).toHaveBeenCalledTimes(1);
  });

  it("holds a __submit__ action even when it carries skip_validation", async () => {
    // The writers see `action: null` for __submit__ and always commit, so
    // the review gate must hold it too.
    const { onsubmit } = renderTargets([
      { label: "Send", value: "__submit__", skip_validation: true },
    ]);
    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onsubmit).not.toHaveBeenCalled();
    expect(tabButtons()[1].getAttribute("aria-selected")).toBe("true");
  });

  it("holds an escape-hatch action that opts back into writing", async () => {
    const { onsubmit } = renderTargets([
      { label: "Save draft", value: "draft", skip_validation: true, writes_targets: true },
    ]);
    await fireEvent.click(screen.getByRole("button", { name: "Save draft" }));
    expect(onsubmit).not.toHaveBeenCalled();
    expect(tabButtons()[1].getAttribute("aria-selected")).toBe("true");
  });
});

// Review finding C-03: for a bridge-served dialog the line showed the raw
// agent-supplied path, which can differ from where the bridge writes. The
// bridge stamps `resolved_path` with its real destination.
describe("Form — the approval line names the real destination (C-03)", () => {
  function line(target: Record<string, unknown>, sessionOrigin?: string) {
    const { container } = render(Form, {
      props: {
        spec: {
          kind: "form",
          title: "Key",
          fields: [{ kind: "text", name: "k", label: "Key", target: target as any }],
        },
        onsubmit: vi.fn(),
        oncancel: vi.fn(),
        sessionOrigin,
      },
    });
    return container.querySelector(".write-target")?.textContent ?? "";
  }

  it("shows the bridge's resolved path for a remote session", () => {
    const text = line(
      { mode: "create", path: "example/token", resolved_path: "/opt/example-app/example/token" },
      "example-host",
    );
    expect(text).toContain("/opt/example-app/example/token");
    expect(text).toContain("example-host");
  });

  it("falls back to the raw path when an older bridge sends none", () => {
    const text = line({ mode: "create", path: "example/token" }, "example-host");
    expect(text).toContain("example/token");
  });
});

// Review finding D-11: the annotation stage answered the pointer only, so a
// `required` annotated_image could not be submitted from the keyboard — the
// only way out was Cancel, which the agent reads as "no".
describe("Form — annotated_image without a pointer (D-11)", () => {
  const IMG = "data:image/png;base64,iVBORw0KGgo=";

  function renderAnn(mode: "point" | "region") {
    const onsubmit = vi.fn();
    const { container } = render(Form, {
      props: {
        spec: {
          kind: "form",
          title: "Mark it",
          fields: [{ kind: "annotated_image", name: "spot", src: IMG, label: "Spot", mode, required: true }],
        },
        onsubmit,
        oncancel: vi.fn(),
      },
    });
    const stage = container.querySelector<HTMLElement>(".annimg-stage")!;
    return { onsubmit, stage };
  }

  it("places, moves and submits a point from the keyboard", async () => {
    const { onsubmit, stage } = renderAnn("point");
    expect(stage.getAttribute("tabindex")).toBe("0");
    expect(stage.getAttribute("role")).toBe("slider");

    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onsubmit).not.toHaveBeenCalled();

    await fireEvent.keyDown(stage, { key: "Enter" });
    await fireEvent.keyDown(stage, { key: "ArrowRight" });
    await fireEvent.keyDown(stage, { key: "ArrowDown", shiftKey: true });
    expect(stage.getAttribute("aria-valuetext")).toContain("52");

    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onsubmit).toHaveBeenCalledTimes(1);
    expect(onsubmit.mock.calls[0][0].values.spot.point).toEqual({ x: 0.52, y: 0.6 });
  });

  it("creates, moves and resizes a region, and Delete clears it", async () => {
    const { onsubmit, stage } = renderAnn("region");

    await fireEvent.keyDown(stage, { key: " " });
    await fireEvent.keyDown(stage, { key: "ArrowLeft" });
    await fireEvent.keyDown(stage, { key: "ArrowRight", shiftKey: true });
    await fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onsubmit.mock.calls[0][0].values.spot.region).toEqual({ x: 0.23, y: 0.25, w: 0.52, h: 0.5 });

    await fireEvent.keyDown(stage, { key: "Delete" });
    expect(stage.getAttribute("aria-valuetext")).toBe("No annotation yet");
  });
});
