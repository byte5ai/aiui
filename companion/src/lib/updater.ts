import { check, Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";

/**
 * Checks the configured endpoint for a new version.
 *
 * Two modes:
 *  • `silent: true` (auto-triggers from App.svelte: post-render event,
 *    window-focus, mount). No prompts, no surfaced UI. If an update is
 *    available AND the dialog window is idle (no pending render), the
 *    update is downloaded and installed transparently followed by a
 *    relaunch. The next agent tool call hits the new version. If the
 *    dialog window is busy with a live form/confirm/ask, the install
 *    is deferred until the next safe moment (catches the
 *    2026-05-06 mid-dialog-popup case AND fixes the v0.4.39
 *    too-silent regression where auto-updates simply never shipped).
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
    // Check whether installing right now would interrupt a live
    // dialog. The Rust side returns false iff `DialogState` has any
    // pending render — in that case we defer until the next
    // auto-check cycle (post-render, window-focus, or mount fires
    // again later). Otherwise we install transparently, no prompt.
    let safe = false;
    try {
      safe = await invoke<boolean>("is_update_safe_to_install");
    } catch (e) {
      console.debug(`[aiui] silent updater safety check failed: ${e}`);
      return;
    }
    if (!safe) {
      console.debug(
        `[aiui] update ${update.version} available — deferred, dialog pending`,
      );
      return;
    }
    console.debug(
      `[aiui] silent install of ${update.version} (dialog idle); relaunching afterwards`,
    );
    try {
      await update.downloadAndInstall();
      await relaunch();
    } catch (e) {
      // Install failure during silent path: don't surface UI (would
      // pop a banner mid-session), just log. Manual "Nach Updates
      // suchen" click can retry with full error reporting.
      console.debug(`[aiui] silent install failed: ${e}`);
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
  await relaunch();
}
