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
});
