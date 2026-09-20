import { check, Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";

/**
 * Checks the configured endpoint for a new version.
 *
 * Two modes:
 *  • `silent: true` (auto-triggered on mount and window-focus from
 *    `setup.ts` and `dialog.ts`). No prompts, no surfaced UI, and — since
 *    v0.4.44 — **no install**: it records the available version via
 *    `set_pending_update` and returns. The settings banner and the system
 *    notification from the headless Rust check are what tell the user.
 *  • `silent: false` (manual button in Settings): full UX —
 *    error/no-update messages, then `downloadAndInstall` + relaunch.
 *
 * #188: this comment used to describe a transparent install-and-relaunch in
 * silent mode, naming `App.svelte` (deleted in the multi-window refactor)
 * as the trigger. That behaviour was removed deliberately — it restarted
 * the app under the user with no indication anything had happened — so the
 * text was describing a mechanism that no longer existed, in the one file a
 * contributor would check first.
 *
 * UX note: use `message()` (single OK button) for pure-info outcomes, and
 * `ask()` (Yes/No) only when the user actually has a decision to make.
 */
export async function checkForUpdates(opts: { silent?: boolean } = {}): Promise<void> {
  let update: Update | null;
  try {
    update = await check();
  } catch (e) {
    if (!opts.silent) {
      await message(`Update-Check fehlgeschlagen:\n${e}`, {
        title: "aiui",
        kind: "warning",
      });
    } else {
      console.debug(`[aiui] silent update check failed: ${e}`);
    }
    return;
  }
  if (!update) {
    if (!opts.silent) {
      await message("Du bist auf der aktuellen Version.", {
        title: "aiui",
        kind: "info",
      });
    }
    return;
  }

  // Update available.
  if (opts.silent) {
    // v0.4.44: notification-first instead of transparent-install. We
    // record the available version in the Rust-side `PendingUpdate`
    // state; Settings.svelte reads that on mount and renders a
    // non-modal banner ("Update auf v{version} verfügbar —
    // Installieren"). Rust also broadcasts an `update:available`
    // event so a live Settings window updates its banner immediately.
    // No `downloadAndInstall` here — the user opts in by clicking
    // the banner (which calls back into this function with
    // `silent: false`), so a long form mid-fill is never interrupted
    // by a sudden restart. The 0.4.43 silent-install path is gone:
    // it solved the mid-dialog UI problem but left the user
    // completely unaware that an update happened, which the user
    // flagged on 2026-05-26.
    try {
      await invoke("set_pending_update", { version: update.version });
      console.debug(
        `[aiui] update ${update.version} available — pending banner set, no install yet`,
      );
    } catch (e) {
      console.debug(`[aiui] failed to record pending update: ${e}`);
    }
    return;
  }

  // Manual path: prompt + install. Promote to Regular activation so
  // the modal actually fronts (Accessory-mode app otherwise loses to
  // Claude Desktop on focus).
  await invoke("surface_for_dialog");

  const wantInstall = await ask(
    `Update auf aiui ${update.version} verfügbar.\n\n${update.body ?? ""}\n\nJetzt installieren?`,
    { title: "aiui Update", kind: "info" },
  );
  if (!wantInstall) return;

  await update.downloadAndInstall();
  // Clear the pending-update banner before relaunch — once the new
  // binary is on disk the banner would be stale (version === current
  // after relaunch, no longer a pending update).
  try {
    await invoke("clear_pending_update");
  } catch (e) {
    console.debug(`[aiui] clear_pending_update failed (continuing): ${e}`);
  }
  // Invariant I1: the host's ExitRequested gate default-denies every
  // Tauri-initiated exit. `relaunch()` fires ExitRequested, so we must latch
  // the exit authority first (case (c), update-restart) or the relaunch would
  // be vetoed and the freshly-installed update would never take effect.
  await invoke("authorize_exit_for_update");
  await relaunch();
}
