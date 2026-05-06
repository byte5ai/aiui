import { check, Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";

/**
 * Checks the configured endpoint for a new version.
 *
 * Two modes:
 *  • `silent: true` (auto-triggers from App.svelte: post-render event,
 *    window-focus, mount). NEVER surfaces UI — neither error toasts,
 *    "you're on latest", nor an install prompt. If a new version is
 *    available the function only logs to console and returns. The user
 *    will see the prompt the next time they manually click "Nach
 *    Updates suchen" in Settings, or when the next GUI restart picks
 *    up the new bundle. Rationale: post-render auto-checks fired in
 *    the middle of an agent's dialog, briefly surfacing the Settings
 *    window and stealing focus from the live `confirm`/`ask`/`form`
 *    the user was answering (reproduced 2026-05-06 right after 0.4.38
 *    shipped — agent's window broke because the silent post-render
 *    check picked up 0.4.38 mid-dialog).
 *  • `silent: false` (manual button in Settings): full UX —
 *    error/no-update messages and the install prompt.
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
    // Defer entirely — no surfacing, no prompt, no policy change.
    // Manual Settings click or next GUI restart will pick it up.
    console.debug(`[aiui] update available (${update.version}) — deferred until manual check or restart`);
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
  await relaunch();
}
