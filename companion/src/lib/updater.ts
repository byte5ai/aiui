import { check, Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ask } from "@tauri-apps/plugin-dialog";

/**
 * Checks the configured endpoint for a new version. If one exists, prompts the
 * user via a native dialog. On confirm: downloads, verifies signature, swaps,
 * relaunches. Silent if already on latest. Call from onMount of the settings
 * window, or wire to a "Check for updates" button.
 */
export async function checkForUpdates(opts: { silent?: boolean } = {}): Promise<void> {
  let update: Update | null;
  try {
    update = await check();
  } catch (e) {
    if (!opts.silent) {
      await ask(`Update-Check fehlgeschlagen:\n${e}`, {
        title: "aiui",
        kind: "warning",
      });
    }
    return;
  }
  if (!update) {
    if (!opts.silent) {
      await ask("Du bist auf der aktuellen Version.", {
        title: "aiui",
        kind: "info",
      });
    }
    return;
  }

  const wantInstall = await ask(
    `Update auf aiui ${update.version} verfügbar.\n\n${update.body ?? ""}\n\nJetzt installieren?`,
    { title: "aiui Update", kind: "info" },
  );
  if (!wantInstall) return;

  await update.downloadAndInstall();
  await relaunch();
}
