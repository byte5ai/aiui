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
function availableUpdate(
  download: () => Promise<void>,
  install: () => Promise<void> = vi.fn().mockResolvedValue(undefined),
) {
  return { version: "0.10.2", body: "release notes", download, install };
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
    const download = vi
      .fn()
      .mockRejectedValue(new Error("signature mismatch"));
    tauri.check.mockResolvedValue(availableUpdate(download));

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
    const download = vi.fn().mockResolvedValue(undefined);
    tauri.check.mockResolvedValue(availableUpdate(download));
    tauri.invoke.mockImplementation(invokeReturning(false));

    const outcome = await checkForUpdates({ silent: false });

    expect(outcome.ok).toBe(true);
    expect(download).not.toHaveBeenCalled();
    expect(tauri.relaunch).not.toHaveBeenCalled();
    // Banner stays: the user is told to finish the dialog and come back.
    expect(invokedCommands()).not.toContain("clear_pending_update");
    expect(tauri.message).toHaveBeenCalledTimes(1);
  });

  it("checks the dialog gate before downloading, and again before relaunching", async () => {
    const order: string[] = [];
    const download = vi.fn(async () => {
      order.push("download");
    });
    const install = vi.fn(async () => {
      order.push("install");
    });
    tauri.check.mockResolvedValue(availableUpdate(download, install));
    tauri.invoke.mockImplementation(async (cmd: string) => {
      order.push(cmd);
      return cmd === "is_update_safe_to_install" ? true : undefined;
    });
    tauri.relaunch.mockImplementation(async () => {
      order.push("relaunch");
    });

    await checkForUpdates({ silent: false });

    const downloaded = order.indexOf("download");
    const gates = order.flatMap((c, i) => (c === "is_update_safe_to_install" ? [i] : []));
    expect(gates).toHaveLength(2);
    // Before the download, so a pending dialog costs no download at all…
    expect(gates[0]).toBeLessThan(downloaded);
    // …and after it (D-08): a download can take long enough for an agent
    // to open a dialog in the meantime.
    expect(gates[1]).toBeGreaterThan(downloaded);
    // …and BEFORE the install: on Windows `install()` exits the process, so
    // a check after it would never run.
    expect(gates[1]).toBeLessThan(order.indexOf("install"));
    expect(order.indexOf("install")).toBeLessThan(order.indexOf("authorize_exit_for_update"));
    // And the exit authority is latched only after the install returned —
    // it is irreversible, so arming it earlier would disarm the host's
    // default-deny exit gate for good (Invariant I1).
    expect(downloaded).toBeLessThan(order.indexOf("authorize_exit_for_update"));
    expect(order.indexOf("authorize_exit_for_update")).toBeLessThan(order.indexOf("relaunch"));
  });

  // D-08: user clicks Install with no dialog open; while the update
  // downloads, an agent opens a form and the user starts typing. Relaunching
  // when the download finishes would destroy the window and the answer.
  it("does not install over a dialog that opened during the download", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockResolvedValue(undefined);
    tauri.check.mockResolvedValue(availableUpdate(download, install));
    let gateCalls = 0;
    tauri.invoke.mockImplementation(async (cmd: string) => {
      if (cmd !== "is_update_safe_to_install") return undefined;
      gateCalls += 1;
      return gateCalls === 1; // safe before the download, not after
    });

    const outcome = await checkForUpdates({ silent: false });

    expect(outcome.ok).toBe(true);
    expect(download).toHaveBeenCalledTimes(1);
    expect(install).not.toHaveBeenCalled();
    expect(tauri.relaunch).not.toHaveBeenCalled();
    // The irreversible exit latch stays unarmed, and the banner stays as the
    // way back to a restart once the dialog is done.
    expect(invokedCommands()).not.toContain("authorize_exit_for_update");
    expect(invokedCommands()).not.toContain("clear_pending_update");
    expect(tauri.message).toHaveBeenCalledTimes(1);
    const [text, options] = tauri.message.mock.calls[0];
    expect(options).toMatchObject({ kind: "info" });
    expect(text).toContain("0.10.2");
    expect(text).toContain("not installed it yet");
  });

  it("does not start a second install while one is in flight", async () => {
    let release: () => void = () => {};
    const download = vi.fn(
      () => new Promise<void>((resolve) => (release = resolve)),
    );
    tauri.check.mockResolvedValue(availableUpdate(download));

    const first = checkForUpdates({ silent: false });
    // Let the first call get as far as the (blocked) install.
    await vi.waitFor(() => expect(download).toHaveBeenCalled());

    await checkForUpdates({ silent: false });
    expect(download).toHaveBeenCalledTimes(1);

    release();
    await first;
  });

  it("leaves the update untouched when the user declines", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    tauri.check.mockResolvedValue(availableUpdate(download));
    tauri.ask.mockResolvedValue(false);

    const outcome = await checkForUpdates({ silent: false });

    expect(outcome.ok).toBe(true);
    expect(download).not.toHaveBeenCalled();
    expect(invokedCommands()).not.toContain("clear_pending_update");
  });
});

describe("checkForUpdates — silent path", () => {
  it("records the pending update and installs nothing", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    tauri.check.mockResolvedValue(availableUpdate(download));

    const outcome = await checkForUpdates({ silent: true });

    expect(outcome.ok).toBe(true);
    expect(invokedCommands()).toEqual(["set_pending_update"]);
    expect(download).not.toHaveBeenCalled();
    expect(tauri.ask).not.toHaveBeenCalled();
    expect(tauri.message).not.toHaveBeenCalled();
  });
});
