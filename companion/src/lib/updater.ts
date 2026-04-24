import { check, Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";

/**
 * Checks the configured endpoint for a new version. If one exists, prompts the
 * user via a native dialog. On confirm: downloads, verifies signature, swaps,
 * relaunches. Silent if already on latest. Call from onMount of the settings
 * window, or wire to a "Check for updates" button.
 *
 * UX note: use `message()` (single OK button) for pure-info outcomes, and
 * `ask()` (Yes/No) only when the user actually has a decision to make. Using
 * `ask()` for "you're on the latest version" produces a nonsensical two-
 * button dialog.
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

  // When running in Accessory mode (auto-spawned from MCP-stdio), macOS
  // won't bring a modal dialog to the front on its own. Promote the app to
  // Regular + surface the window so the user actually sees the prompt.
  await invoke("surface_for_dialog");

  const wantInstall = await ask(
    `Update auf aiui ${update.version} verfügbar.\n\n${update.body ?? ""}\n\nJetzt installieren?`,
    { title: "aiui Update", kind: "info" },
  );
  if (!wantInstall) return;

  await update.downloadAndInstall();
  await relaunch();
}
