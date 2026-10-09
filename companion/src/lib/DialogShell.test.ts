// Review finding D-02: the frontend's TTL auto-cancel.
//
// The shell cancels a few seconds before the backend's own TTL sweep so a
// last-second submit cannot race it. Two things went wrong with that:
//
//  (a) it went out as a plain `dialog_cancel({id})` — the very shape an agent
//      is told means "the user actively said no, do not re-ask". A dialog
//      nobody answered for two hours was reported as a refusal.
//  (b) the local deadline is wall-clock, Rust's TTL clock is monotonic and
//      stands still while the Mac sleeps. On wake the overdue tick cancelled
//      before the resync could rebase the deadline — a dialog the backend
//      still had hours left on vanished as the lid opened.
//
// No DialogShell test existed before; the deadline, the resync and the
// auto-cancel were all unpinned.

import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "../i18n";

const invoke = vi.hoisted(() => vi.fn());
/** The global, any-window `listen` — must not carry the liveness probe. */
const globalListen = vi.hoisted(() => vi.fn(() => Promise.resolve(() => {})));
/** `listen` on this dialog's own webview window. */
const windowListeners = vi.hoisted(() => new Map<string, (e: { payload: unknown }) => void>());
const windowListen = vi.hoisted(() =>
  vi.fn((name: string, cb: (e: { payload: unknown }) => void) => {
    windowListeners.set(name, cb);
    return Promise.resolve(() => windowListeners.delete(name));
  }),
);
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: globalListen }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ label: "dlg-1" }),
}));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ label: "dlg-1", listen: windowListen }),
}));

const { default: DialogShell } = await import("./DialogShell.svelte");

/** What Rust reports as left on the dialog, in seconds; `null` = resolved. */
let backendLeft: number | null;
/** `remaining_secs` in the pulled spec. */
let pulledLeft: number;

function cancels() {
  return invoke.mock.calls.filter((c) => c[0] === "dialog_cancel");
}

async function settle() {
  for (let i = 0; i < 8; i++) {
    await Promise.resolve();
    await tick();
  }
}

async function mounted() {
  const view = render(DialogShell);
  await settle();
  expect(view.container.textContent).toContain("Run the migration?");
  return view;
}

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset();
  globalListen.mockClear();
  windowListen.mockClear();
  windowListeners.clear();
  backendLeft = 7200;
  pulledLeft = 7200;
  invoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "get_dialog_spec":
        return {
          id: "dlg-1",
          spec: { kind: "confirm", title: "Run the migration?", destructive: true },
          ttl_secs: 7200,
          remaining_secs: pulledLeft,
        };
      case "get_dialog_remaining":
        return backendLeft;
      default:
        return undefined;
    }
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("DialogShell — TTL expiry", () => {
  it("tells the agent the dialog timed out, not that the user said no", async () => {
    pulledLeft = 20;
    backendLeft = 20;
    await mounted();

    // Both clocks agree: 16 s later there are 4 s left, inside the lead.
    backendLeft = 4;
    await vi.advanceTimersByTimeAsync(16_000);
    await settle();

    expect(cancels()).toHaveLength(1);
    expect(cancels()[0][1]).toEqual({ id: "dlg-1", reason: "ttl_expired" });
  });

  it("does not cancel on wake when the backend clock paused during sleep", async () => {
    const { container } = await mounted();

    // Three hours of sleep: the wall clock jumps, no timer fires meanwhile,
    // and Rust's monotonic TTL clock did not advance at all.
    vi.setSystemTime(Date.now() + 3 * 60 * 60 * 1000);
    backendLeft = 7190;
    await vi.advanceTimersByTimeAsync(1_000);
    await settle();

    expect(cancels()).toHaveLength(0);
    expect(container.textContent).toContain("Run the migration?");
    // And the countdown is rebased on what Rust said, not stuck at zero.
    expect(container.querySelector(".ttl-banner")).toBeNull();
  });

  it("leaves the cancel to the backend when it cannot ask", async () => {
    pulledLeft = 3;
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_dialog_spec") {
        return {
          id: "dlg-1",
          spec: { kind: "confirm", title: "Run the migration?" },
          ttl_secs: 7200,
          remaining_secs: pulledLeft,
        };
      }
      if (cmd === "get_dialog_remaining") throw new Error("ipc down");
      return undefined;
    });
    await mounted();
    await vi.advanceTimersByTimeAsync(2_000);
    await settle();

    expect(cancels()).toHaveLength(0);
  });

  it("keeps a user's own cancel reason-less", async () => {
    await mounted();
    await fireEvent.keyDown(window, { key: "Escape" });
    await settle();

    expect(cancels()).toHaveLength(1);
    expect(cancels()[0][1]).toEqual({ id: "dlg-1" });
  });
});

// Review finding A-08: `/health` pings ONE dialog window and times the pong.
// With the global (any-target) `listen`, every dialog window received that
// ping and the first to answer won — a frozen newest window was reported
// healthy as long as an older one was not.
describe("DialogShell — liveness probe (A-08)", () => {
  it("answers ui:ping on its own window only", async () => {
    await mounted();

    expect(windowListen).toHaveBeenCalledWith("ui:ping", expect.any(Function));
    expect(globalListen.mock.calls.some((c) => (c as unknown[])[0] === "ui:ping")).toBe(false);

    windowListeners.get("ui:ping")!({ payload: "probe-1" });
    await settle();
    expect(invoke).toHaveBeenCalledWith("ui_pong", { id: "probe-1" });
  });

  it("stops answering once the dialog is gone", async () => {
    const view = await mounted();
    view.unmount();
    await settle();
    expect(windowListeners.has("ui:ping")).toBe(false);
  });
});
