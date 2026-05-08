// Per-window lifecycle wiring shared by both entry points (setup + dialog).
//
// Centralised here because the cooldown is per-window (each Tauri window
// is its own JS VM, with its own `lastUpdateCheck` state) and we want
// every window to participate in the same trigger set without copy-paste:
//
//   • on first mount (initial check at GUI start),
//   • on `update:check` event from Rust (fired after each successful render —
//     clusters around real user activity),
//   • on window focus (covers wake-from-sleep and "user came back to the
//     Mac" without needing an OS-level event hook).
//
// A 30-minute cooldown debounces bursts so a chatty session doesn't
// hammer the GitHub release endpoint. Both windows share the cooldown
// shape independently — the duplicate hits are still well below GitHub's
// rate limit.

import { listen } from "@tauri-apps/api/event";
import { checkForUpdates } from "./updater";

const UPDATE_COOLDOWN_MS = 30 * 60 * 1000;
let lastUpdateCheck = 0;

function maybeCheckForUpdates(reason: string) {
  const now = Date.now();
  if (now - lastUpdateCheck < UPDATE_COOLDOWN_MS) return;
  lastUpdateCheck = now;
  console.debug(`[aiui] update check (${reason})`);
  void checkForUpdates({ silent: true });
}

/**
 * Install update-check triggers for the current window. Returns a
 * teardown function that unbinds the listeners — entry points pass it
 * back through Svelte's `onMount` cleanup.
 */
export function installUpdateChecks(): () => void {
  const onFocus = () => maybeCheckForUpdates("window-focus");

  const unUpdate = listen<string>("update:check", (e) => {
    maybeCheckForUpdates(`rust:${e.payload}`);
  });

  window.addEventListener("focus", onFocus);

  // Initial check on mount.
  maybeCheckForUpdates("startup");

  return () => {
    void unUpdate.then((u) => u());
    window.removeEventListener("focus", onFocus);
  };
}
