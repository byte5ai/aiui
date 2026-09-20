import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { get } from "svelte/store";
import { _, waitLocale } from "svelte-i18n";

/** What a check/install attempt did. `ok: false` always carries an `error`
 *  and has already been surfaced to the user in a native modal; the caller
 *  can additionally push it into an inline log. #197: before this, every
 *  failure of the install chain resolved into a `finally` that cleared
 *  `busy` and said nothing, so a dead download, a signature mismatch, a full
 *  disk and a cancelled admin prompt all looked exactly like "nothing
 *  happened" — the one support signature a maintainer cannot work with. */
export type UpdateOutcome = { ok: boolean; error?: string };

/** Re-entrancy latch for the manual path (#197). Two concurrent
 *  `downloadAndInstall()` calls race over the same bundle — on macOS
 *  `install_inner` renames the extracted `.app` over the live one. Scope
 *  note: each Tauri window is its own JS VM, so this guards one window.
 *  That is sufficient, because the manual path is only reachable from
 *  Settings, which lives in the single setup window. */
let installInFlight = false;

/** `updater.ts` is a plain module, not a component, so the `$_` auto-
 *  subscription isn't available — read the store directly. */
function tr(key: string, values?: Record<string, string | number>): string {
  return get(_)(key, values ? { values } : undefined);
}

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
 *    error/no-update messages, the Invariant I5 safety gate, then
 *    `downloadAndInstall` + relaunch.
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
export async function checkForUpdates(
  opts: { silent?: boolean } = {},
): Promise<UpdateOutcome> {
  // The dialog windows call this early in mount, so the locale may not be
  // resolved yet — and every string below lands in a *native OS modal*,
  // the most prominent text the product shows (#197).
  // Never let a locale hiccup take the update path down with it — a
  // missing translation degrades to the key, a thrown one would swallow
  // the whole check.
  await waitLocale().catch((e) => console.debug(`[aiui] waitLocale: ${e}`));

  if (!opts.silent) {
    if (installInFlight) {
      console.debug("[aiui] update check already in flight, ignoring re-entry");
      return { ok: true };
    }
    installInFlight = true;
  }
  try {
    return await run(opts);
  } finally {
    if (!opts.silent) installInFlight = false;
  }
}

async function run(opts: { silent?: boolean }): Promise<UpdateOutcome> {
  let update: Update | null;
  try {
    update = await check();
  } catch (e) {
    const error = String(e);
    if (!opts.silent) {
      await message(tr("settings.updates.check_failed", { error }), {
        title: "aiui",
        kind: "warning",
      });
    } else {
      console.debug(`[aiui] silent update check failed: ${error}`);
    }
    return { ok: false, error };
  }
  if (!update) {
    // #197: "no update available" has to retract the banner, on both the
    // silent and the manual path. Without this, a yanked release — or a
    // `latest.json` that stops advertising this platform — left a banner
    // the user could not dismiss, whose Install button answered "you are on
    // the current version" and changed nothing, forever.
    await clearPendingUpdate();
    if (!opts.silent) {
      await message(tr("settings.updates.up_to_date"), {
        title: "aiui",
        kind: "info",
      });
    }
    return { ok: true };
  }

  // Update available.
  if (opts.silent) {
    // v0.4.44: notification-first instead of transparent-install. We
    // record the available version in the Rust-side `PendingUpdate`
    // state; Settings.svelte reads that on mount and renders a
    // non-modal banner. Rust also broadcasts an `update:available`
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
        `[aiui] update ${update.version} available: pending banner set, no install yet`,
      );
    } catch (e) {
      console.debug(`[aiui] failed to record pending update: ${e}`);
    }
    return { ok: true };
  }

  // Manual path: prompt + install. Everything from here to `relaunch()` is
  // inside one try/catch — `downloadAndInstall()` rejects on a dead network,
  // a minisign signature mismatch, a full disk, and on the admin prompt the
  // plugin raises when the install location isn't writable, and each of
  // those has to reach the user (#197).
  try {
    // Promote to Regular activation so the modal actually fronts
    // (Accessory-mode app otherwise loses to Claude Desktop on focus).
    await invoke("surface_for_dialog");

    const wantInstall = await ask(
      `${tr("settings.updates.available", { version: update.version })}\n\n` +
        `${update.body ?? ""}\n\n${tr("settings.updates.install_question")}`,
      { title: tr("settings.updates.title"), kind: "info" },
    );
    if (!wantInstall) return { ok: true };

    // Invariant I5 (#197): installing means relaunching, and relaunching
    // under a pending dialog destroys that window and its in-flight
    // `/render` — the user's half-filled form is gone and the agent on the
    // other end gets a cancelled result. `is_update_safe_to_install` is the
    // command written for exactly this; between v0.4.44 and #197 nothing
    // called it. The banner stays in place, so the user can install again
    // once they have finished what they were doing.
    if (!(await updateIsSafeToInstall())) {
      await message(tr("settings.updates.deferred_dialog_open"), {
        title: "aiui",
        kind: "info",
      });
      return { ok: true };
    }

    await update.downloadAndInstall();

    // Everything below runs on macOS/Linux ONLY. On Windows
    // `tauri-plugin-updater` hands the NSIS installer to `ShellExecuteW` and
    // then calls `std::process::exit(0)` *inside* the await above, so this
    // process is already gone. The Windows equivalents of these two steps
    // (latching the exit authority, sweeping the ssh-NTR children) are wired
    // to the plugin's `on_before_exit` hook in `lib.rs`, which the plugin
    // invokes only on that branch (#197).

    // Clear the pending-update banner before relaunch — once the new
    // binary is on disk the banner would be stale (version === current
    // after relaunch, no longer a pending update).
    await clearPendingUpdate();
    // Invariant I1: the host's ExitRequested gate default-denies every
    // Tauri-initiated exit. `relaunch()` fires ExitRequested, so we must latch
    // the exit authority first (case (c), update-restart) or the relaunch would
    // be vetoed and the freshly-installed update would never take effect.
    // Latched *after* the install on purpose: `ExitAuthority::authorize()` is
    // irreversible, so arming it in front of an install that can still fail
    // would leave a live host with its exit gate permanently disarmed.
    await invoke("authorize_exit_for_update");
    await relaunch();
    return { ok: true };
  } catch (e) {
    const error = String(e);
    console.debug(`[aiui] update install failed: ${error}`);
    await message(tr("settings.updates.install_failed", { error }), {
      title: "aiui",
      kind: "error",
    });
    return { ok: false, error };
  }
}

/** The I5 gate. An IPC failure here means the command layer itself is
 *  broken, in which case dialogs aren't rendering either — so we fail open
 *  rather than making a transport blip permanently un-updatable. */
async function updateIsSafeToInstall(): Promise<boolean> {
  try {
    return await invoke<boolean>("is_update_safe_to_install");
  } catch (e) {
    console.debug(`[aiui] is_update_safe_to_install failed (continuing): ${e}`);
    return true;
  }
}

/** Best-effort: a stuck banner is a nuisance, not a reason to abort an
 *  install that already succeeded. */
async function clearPendingUpdate(): Promise<void> {
  try {
    await invoke("clear_pending_update");
  } catch (e) {
    console.debug(`[aiui] clear_pending_update failed (continuing): ${e}`);
  }
}
