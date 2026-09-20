// Regression cover for issue #208 — the four defects of the settings pane.
//
// The companion had no frontend test runner before this, and two of the
// four defects are pure frontend behaviour: a 2 s status poll that outlived
// the window it belonged to (the setup window is hidden on close, never
// destroyed, so `onDestroy` never runs), and a health banner that told four
// unrelated failures the same story — including immediately after Uninstall,
// where it claimed a port squatter while aiui's own server was still
// listening. Neither is reachable from `cargo test`.
//
// What is asserted here is behaviour a user could observe: how many times
// `status` is invoked, what is on screen, and which command a click sends.

import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, fireEvent, cleanup } from "@testing-library/svelte";
import { addMessages, init } from "svelte-i18n";
import en from "../i18n/en.json";

const invoke = vi.hoisted(() => vi.fn());
const eventListeners = vi.hoisted(
  () => new Map<string, (e: { payload: unknown }) => void>(),
);

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    eventListeners.set(name, cb);
    return Promise.resolve(() => eventListeners.delete(name));
  },
}));
// Keeps the updater plugin (and its Tauri IPC) out of the component graph.
vi.mock("./updater", () => ({ checkForUpdates: vi.fn() }));

addMessages("en", en);
init({ fallbackLocale: "en", initialLocale: "en" });

// Imported after the mocks are registered so the component picks them up.
const { default: Settings } = await import("./Settings.svelte");

type StatusOverrides = Partial<Record<string, unknown>>;

let current: Record<string, unknown>;

function statusReport(over: StatusOverrides = {}) {
  return {
    app_binary_path: "/Applications/aiui.app/Contents/MacOS/aiui",
    token_path: "/Users/u/.config/aiui/token",
    http_port: 7777,
    claude_config_ok: true,
    claude_code_config_ok: true,
    skill_installed: true,
    claude_desktop_running: true,
    remotes: [] as string[],
    tunnels: {} as Record<string, unknown>,
    build_info: "aiui v0.10.1",
    welcome_pending: false,
    http_error: null as string | null,
    http_alive: true,
    http_probe_reason: "alive",
    http_hint_key: "settings.http_error.hint.macos",
    os: "macos",
    pending_update: null as string | null,
    ...over,
  };
}

function statusCalls() {
  return invoke.mock.calls.filter((c) => c[0] === "status").length;
}

function setHidden(hidden: boolean) {
  Object.defineProperty(document, "hidden", {
    configurable: true,
    get: () => hidden,
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

/** Let the component's promise chain and Svelte's scheduler settle. */
async function settle() {
  for (let i = 0; i < 4; i++) {
    await Promise.resolve();
    await tick();
  }
}

async function mounted(over: StatusOverrides = {}) {
  current = statusReport(over);
  const view = render(Settings);
  await settle();
  return view;
}

beforeEach(() => {
  vi.useFakeTimers();
  eventListeners.clear();
  current = statusReport();
  invoke.mockReset();
  invoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "status":
        return current;
      case "uninstall_all":
        // What the real command does to the state the pane reads: the
        // token file and `first_run_done` are gone, so the probe can no
        // longer authenticate and onboarding re-arms itself (#208).
        current = statusReport({
          ...current,
          http_alive: false,
          http_probe_reason: "token_unreadable",
          http_hint_key: "settings.http_error.hint.token",
          welcome_pending: true,
          remotes: [],
        });
        return [{ ok: true, message: "Lokale Dateien entfernt", details: null }];
      case "remove_remote":
        return [{ ok: true, message: "Host entfernt", details: null }];
      default:
        return [];
    }
  });
});

afterEach(() => {
  // Explicit, not auto-cleanup: the component never unmounts itself here
  // either, and a leftover instance keeps its own poll running into the
  // next test — which is the very thing under test.
  cleanup();
  vi.useRealTimers();
  setHidden(false);
});

describe("the status poll", () => {
  it("stops while the document is hidden and resumes when it is not", async () => {
    await mounted();
    expect(statusCalls()).toBe(1);

    await vi.advanceTimersByTimeAsync(2000);
    expect(statusCalls()).toBe(2);

    setHidden(true);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(statusCalls()).toBe(2);

    setHidden(false);
    await settle();
    expect(statusCalls()).toBe(3);
    await vi.advanceTimersByTimeAsync(2000);
    expect(statusCalls()).toBe(4);
  });

  it("stops when Rust reports the setup window hidden", async () => {
    // The load-bearing path on WKWebView: the window is hidden rather
    // than destroyed (Invariant I2), and `visibilityState` can keep
    // reporting "visible" for it.
    await mounted();
    const onVisibility = eventListeners.get("setup:visibility");
    expect(onVisibility, "Settings subscribes to setup:visibility").toBeTypeOf(
      "function",
    );

    onVisibility!({ payload: false });
    const before = statusCalls();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(statusCalls()).toBe(before);

    onVisibility!({ payload: true });
    await settle();
    expect(statusCalls()).toBe(before + 1);
  });

  it("does not force the expensive probes on a poll tick", async () => {
    // The poll must leave the Rust-side 15 s cache alone, or the cache
    // buys nothing: it is what keeps `pgrep`/`tasklist` to four spawns a
    // minute while the window is open.
    await mounted();
    await vi.advanceTimersByTimeAsync(6000);
    const polls = invoke.mock.calls.filter((c) => c[0] === "status").slice(1);
    expect(polls.length).toBeGreaterThan(0);
    for (const call of polls) {
      expect(call[1]).toEqual({ force: false });
    }
  });

  it("stops for good once the uninstall has run", async () => {
    const { getByText } = await mounted();
    await fireEvent.click(getByText("Uninstall"));
    await fireEvent.click(getByText("Remove everything"));
    await settle();

    const after = statusCalls();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(statusCalls()).toBe(after);
  });
});

describe("the health banner", () => {
  it("waits for three consecutive failures", async () => {
    const { container } = await mounted({ http_alive: true });
    current = statusReport({ http_alive: false, http_probe_reason: "unreachable" });

    await vi.advanceTimersByTimeAsync(2000);
    expect(container.querySelector(".http-error")).toBeNull();
    await vi.advanceTimersByTimeAsync(2000);
    expect(container.querySelector(".http-error")).toBeNull();
    await vi.advanceTimersByTimeAsync(2000);
    expect(container.querySelector(".http-error")).not.toBeNull();
  });

  it("forgets the failures again after one success", async () => {
    const { container } = await mounted({ http_alive: true });
    current = statusReport({ http_alive: false });
    await vi.advanceTimersByTimeAsync(4000);
    current = statusReport({ http_alive: true });
    await vi.advanceTimersByTimeAsync(2000);
    current = statusReport({ http_alive: false });
    await vi.advanceTimersByTimeAsync(4000);
    expect(container.querySelector(".http-error")).toBeNull();
  });

  it("renders the hint the backend chose, not a port-squatter claim", async () => {
    const { container } = await mounted({
      http_alive: false,
      http_probe_reason: "token_unreadable",
      http_hint_key: "settings.http_error.hint.token",
    });
    await vi.advanceTimersByTimeAsync(6000);
    const hint = container.querySelector(".http-error-hint");
    expect(hint?.textContent).toContain("auth token");
    expect(hint?.textContent).not.toContain("7777");
  });

  it("stays away after an uninstall, and so does the welcome wizard", async () => {
    // The deterministic case: uninstall deletes the token file and
    // `first_run_done`, so the pane used to contradict itself in red the
    // moment the done-modal was dismissed.
    const { getByText, container } = await mounted({ welcome_pending: true });
    expect(container.querySelector(".welcome")).not.toBeNull();

    await fireEvent.click(getByText("Uninstall"));
    await fireEvent.click(getByText("Remove everything"));
    await settle();
    await vi.advanceTimersByTimeAsync(30_000);

    expect(container.querySelector(".http-error")).toBeNull();
    expect(container.querySelector(".welcome")).toBeNull();
  });
});

describe("removing a remote host", () => {
  const withHost = {
    remotes: ["devhost"],
    tunnels: { devhost: { state: "connected" } },
  };

  it("takes two clicks, and Back cancels", async () => {
    const { getByText, queryByText } = await mounted(withHost);

    await fireEvent.click(getByText("Remove"));
    await settle();
    expect(invoke.mock.calls.some((c) => c[0] === "remove_remote")).toBe(false);
    expect(queryByText(/Really remove devhost/)).not.toBeNull();

    await fireEvent.click(getByText("Back"));
    await settle();
    expect(queryByText(/Really remove devhost/)).toBeNull();
    expect(invoke.mock.calls.some((c) => c[0] === "remove_remote")).toBe(false);

    await fireEvent.click(getByText("Remove"));
    await settle();
    await fireEvent.click(getByText("Remove host"));
    await settle();

    const removals = invoke.mock.calls.filter((c) => c[0] === "remove_remote");
    expect(removals).toHaveLength(1);
    expect(removals[0][1]).toEqual({ hostAlias: "devhost" });
  });
});

describe("the demo prompt", () => {
  it("falls back to a readable textarea when no clipboard works", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "status") return current;
      if (cmd === "copy_to_clipboard") throw new Error("no clipboard plugin");
      return [];
    });
    // jsdom has no `navigator.clipboard`, which is exactly the shipped
    // failure: `navigator.clipboard.writeText` throws.
    const { getByText, container } = await mounted({ welcome_pending: true });

    await fireEvent.click(getByText("Copy demo prompt"));
    await settle();

    const area = container.querySelector<HTMLTextAreaElement>("textarea[readonly]");
    expect(area, "the prompt itself is the fallback").not.toBeNull();
    expect(area!.value).toContain("aiui demo");
    expect(container.querySelector(".log-line.err")?.textContent).toContain(
      "clipboard",
    );
  });

  it("reports success without the fallback when the command works", async () => {
    const { getByText, container } = await mounted({ welcome_pending: true });
    await fireEvent.click(getByText("Copy demo prompt"));
    await settle();

    expect(container.querySelector("textarea[readonly]")).toBeNull();
    expect(
      invoke.mock.calls.filter((c) => c[0] === "copy_to_clipboard"),
    ).toHaveLength(1);
  });
});
