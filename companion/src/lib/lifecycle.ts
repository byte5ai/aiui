// Per-window lifecycle wiring for the **setup window only** (#195).
//
// The dialog window used to install this too. It no longer does: it renders
// agent-authored content, so `capabilities/dialog.json` grants it neither
// `updater:*` (for `check()`) nor the event permission this listener needs,
// and the headless 6 h updater task in `run()` (lib.rs) already detects new
// releases independently of any window. Calling it from there produced two
// unhandled promise rejections per render — `plugin:event|listen` and
// `plugin:updater|check`, both "not allowed by ACL" — for a trigger set that
// was redundant. Keep this import out of `dialog.ts`.
//
// The cooldown is per-window (each Tauri window is its own JS VM, with its
// own `lastUpdateCheck` state), and the triggers are:
//
//   • on first mount (initial check at GUI start),
//   • on `update:check` event from Rust (fired after each successful render —
//     clusters around real user activity),
//   • on window focus (covers wake-from-sleep and "user came back to the
//     Mac" without needing an OS-level event hook).
//
// v0.4.44 raised the cooldown from 30 min to 6 h. The check itself is
// cheap, but with the new notification-first model (silent path sets a
// pending-update flag instead of installing) we don't need bursty
// re-checks — once the flag is set, the banner stays until the user
// installs or the version on disk catches up. Six hours is the
// "approximate daily" rhythm the user asked for on 2026-05-26: a
// chatty session won't hammer the GitHub release endpoint, but a
// long-running GUI still picks up new releases the same day.

import { listen } from "@tauri-apps/api/event";
import { checkForUpdates } from "./updater";

const UPDATE_COOLDOWN_MS = 6 * 60 * 60 * 1000;
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
 *
 * `setup.ts` is the only caller, and must stay the only one (#195).
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
