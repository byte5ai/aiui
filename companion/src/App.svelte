<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { onMount } from "svelte";
  import Settings from "./lib/Settings.svelte";
  import DialogShell from "./lib/DialogShell.svelte";
  import { checkForUpdates } from "./lib/updater";

  // Tauri 2 multi-window: the same Vite bundle is loaded into both the
  // setup and the dialog windows. We branch the *contents* on the
  // window label, so each window sees exactly the listeners and DOM it
  // needs — no cross-window state, no `dialog:show` leaking into
  // settings, no settings UI flashing during a dialog.
  let label = $state<string | null>(null);

  // Update checks are lifecycle-driven, not interval-driven. Triggers:
  //  • on mount (initial check at GUI start),
  //  • on `update:check` event from Rust (fired after each successful
  //    render — clusters around real user activity),
  //  • on window focus (covers wake-from-sleep and "user came back to
  //    the Mac" without needing an OS-level event hook).
  // A 30-minute cooldown debounces bursts so a chatty session doesn't
  // hammer the GitHub release endpoint.
  const UPDATE_COOLDOWN_MS = 30 * 60 * 1000;
  let lastUpdateCheck = 0;

  function maybeCheckForUpdates(reason: string) {
    const now = Date.now();
    if (now - lastUpdateCheck < UPDATE_COOLDOWN_MS) return;
    lastUpdateCheck = now;
    console.debug(`[aiui] update check (${reason})`);
    void checkForUpdates({ silent: true });
  }

  function onFocus() {
    maybeCheckForUpdates("window-focus");
  }

  onMount(() => {
    label = getCurrentWebviewWindow().label;

    // Update-check trigger from Rust — fired after each successful
    // render. Frontend gates with the cooldown so we don't actually hit
    // GitHub on every render. Both windows can hear this; first one
    // through the cooldown wins.
    const unUpdate = listen<string>("update:check", (e) => {
      maybeCheckForUpdates(`rust:${e.payload}`);
    });

    window.addEventListener("focus", onFocus);

    // Initial check on mount.
    maybeCheckForUpdates("startup");

    return async () => {
      (await unUpdate)();
      window.removeEventListener("focus", onFocus);
    };
  });
</script>

<!-- The drag-region sits at the very top, OVER the macOS overlay
     title bar. Tauri lets the traffic-light buttons capture clicks
     even when our region overlaps them, so the user can grab the
     window from anywhere along the top edge except directly on the
     buttons. Position fixed so it doesn't push container content
     down. -->
<div class="drag-region" data-tauri-drag-region></div>

<!-- Container provides the Apple-HIG-compliant 44 px top breathing
     room so content never collides with the traffic-light buttons,
     plus side padding and scroll behaviour. Both setup and dialog
     windows share this chrome. -->
<main class="container">
  {#if label === "setup"}
    <Settings />
  {:else if label === "dialog"}
    <DialogShell />
  {/if}
  <!-- During the first paint label is null — render nothing rather
       than guess wrong. Tauri sets the label synchronously, so this
       lasts only one micro-task. -->
</main>

<style>
  .drag-region {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    height: 28px;
    z-index: 1;
  }
</style>
