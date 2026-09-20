// #197 regression cover for the in-app update path.
//
// Every assertion here corresponds to a failure that shipped: an install
// that died with no message at all, a banner that could not be retracted,
// an install that ran over a live dialog, and a button whose `disabled`
// attribute did not actually stop a second concurrent install.
//
// The Tauri plugins are mocked wholesale — none of them has a meaning
// outside a WebView — but `svelte-i18n` is deliberately NOT mocked: the
// point is partly that these modals now carry real, resolvable keys.

import { beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({
  check: vi.fn(),
  relaunch: vi.fn(),
  ask: vi.fn(),
  message: vi.fn(),
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-updater", () => ({ check: tauri.check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: tauri.relaunch }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  ask: tauri.ask,
  message: tauri.message,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));

import "../i18n";
import { checkForUpdates } from "./updater";

/** A pending update the user then confirms in the native prompt. */
function availableUpdate(downloadAndInstall: () => Promise<void>) {
  return { version: "0.10.2", body: "release notes", downloadAndInstall };
}

/** Rust commands answer `undefined` unless a test says otherwise; the I5
 *  gate is the only one with a meaningful return value. */
function invokeReturning(safeToInstall: boolean) {
  return vi.fn(async (cmd: string) =>
    cmd === "is_update_safe_to_install" ? safeToInstall : undefined,
  );
}

function invokedCommands(): string[] {
  return tauri.invoke.mock.calls.map((c) => c[0] as string);
}

beforeEach(() => {
  vi.clearAllMocks();
  tauri.invoke.mockImplementation(invokeReturning(true));
  tauri.ask.mockResolvedValue(true);
  tauri.message.mockResolvedValue(undefined);
  tauri.relaunch.mockResolvedValue(undefined);
});

describe("checkForUpdates — manual path", () => {
  it("surfaces a failed install instead of swallowing it", async () => {
    const downloadAndInstall = vi
      .fn()
      .mockRejectedValue(new Error("signature mismatch"));
    tauri.check.mockResolvedValue(availableUpdate(downloadAndInstall));

    const outcome = await checkForUpdates({ silent: false });

    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("signature mismatch");
    expect(tauri.message).toHaveBeenCalledTimes(1);
    const [text, options] = tauri.message.mock.calls[0];
    expect(options).toMatchObject({ kind: "error" });
    expect(text).toContain("signature mismatch");
    // A failed install must not latch the irreversible exit authority or
    // relaunch into a binary that was never written.
    expect(invokedCommands()).not.toContain("authorize_exit_for_update");
    expect(tauri.relaunch).not.toHaveBeenCalled();
  });

  it("reports a failed update check rather than returning silently", async () => {
    tauri.check.mockRejectedValue(new Error("endpoint unreachable"));

    const outcome = await checkForUpdates({ silent: false });

    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("endpoint unreachable");
    expect(tauri.message).toHaveBeenCalledTimes(1);
  });

  it("clears the pending-update banner when no update is offered", async () => {
    tauri.check.mockResolvedValue(null);

    const outcome = await checkForUpdates({ silent: false });

    expect(outcome.ok).toBe(true);
    // The yanked-release case: the banner said 0.10.2 was available, the
    // feed no longer offers it, and without this the banner was permanent.
    expect(invokedCommands()).toContain("clear_pending_update");
  });

  it("clears the banner on the silent path too", async () => {
    tauri.check.mockResolvedValue(null);

    await checkForUpdates({ silent: true });

    expect(invokedCommands()).toContain("clear_pending_update");
    expect(tauri.message).not.toHaveBeenCalled();
  });

  it("does not install while a dialog is still pending", async () => {
    const downloadAndInstall = vi.fn().mockResolvedValue(undefined);
    tauri.check.mockResolvedValue(availableUpdate(downloadAndInstall));
    tauri.invoke.mockImplementation(invokeReturning(false));

    const outcome = await checkForUpdates({ silent: false });

    expect(outcome.ok).toBe(true);
    expect(downloadAndInstall).not.toHaveBeenCalled();
    expect(tauri.relaunch).not.toHaveBeenCalled();
    // Banner stays: the user is told to finish the dialog and come back.
    expect(invokedCommands()).not.toContain("clear_pending_update");
    expect(tauri.message).toHaveBeenCalledTimes(1);
  });

  it("checks the dialog gate before downloading, not after", async () => {
    const order: string[] = [];
    const downloadAndInstall = vi.fn(async () => {
      order.push("downloadAndInstall");
    });
    tauri.check.mockResolvedValue(availableUpdate(downloadAndInstall));
    tauri.invoke.mockImplementation(async (cmd: string) => {
      order.push(cmd);
      return cmd === "is_update_safe_to_install" ? true : undefined;
    });

    await checkForUpdates({ silent: false });

    expect(order.indexOf("is_update_safe_to_install")).toBeGreaterThanOrEqual(0);
    expect(order.indexOf("is_update_safe_to_install")).toBeLessThan(
      order.indexOf("downloadAndInstall"),
    );
    // And the exit authority is latched only after the install returned —
    // it is irreversible, so arming it earlier would disarm the host's
    // default-deny exit gate for good (Invariant I1).
    expect(order.indexOf("downloadAndInstall")).toBeLessThan(
      order.indexOf("authorize_exit_for_update"),
    );
  });

  it("does not start a second install while one is in flight", async () => {
    let release: () => void = () => {};
    const downloadAndInstall = vi.fn(
      () => new Promise<void>((resolve) => (release = resolve)),
    );
    tauri.check.mockResolvedValue(availableUpdate(downloadAndInstall));

    const first = checkForUpdates({ silent: false });
    // Let the first call get as far as the (blocked) install.
    await vi.waitFor(() => expect(downloadAndInstall).toHaveBeenCalled());

    await checkForUpdates({ silent: false });
    expect(downloadAndInstall).toHaveBeenCalledTimes(1);

    release();
    await first;
  });

  it("leaves the update untouched when the user declines", async () => {
    const downloadAndInstall = vi.fn().mockResolvedValue(undefined);
    tauri.check.mockResolvedValue(availableUpdate(downloadAndInstall));
    tauri.ask.mockResolvedValue(false);

    const outcome = await checkForUpdates({ silent: false });

    expect(outcome.ok).toBe(true);
    expect(downloadAndInstall).not.toHaveBeenCalled();
    expect(invokedCommands()).not.toContain("clear_pending_update");
  });
});

describe("checkForUpdates — silent path", () => {
  it("records the pending update and installs nothing", async () => {
    const downloadAndInstall = vi.fn().mockResolvedValue(undefined);
    tauri.check.mockResolvedValue(availableUpdate(downloadAndInstall));

    const outcome = await checkForUpdates({ silent: true });

    expect(outcome.ok).toBe(true);
    expect(invokedCommands()).toEqual(["set_pending_update"]);
    expect(downloadAndInstall).not.toHaveBeenCalled();
    expect(tauri.ask).not.toHaveBeenCalled();
    expect(tauri.message).not.toHaveBeenCalled();
  });
});
