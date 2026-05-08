// Entry point for the **setup** window only. Loaded by `setup.html`,
// which is the URL Rust passes to `WebviewWindowBuilder` for the
// settings-window. Mounts `Settings.svelte` directly — there is no
// runtime branch on the window label any more (the legacy `App.svelte`
// did that for both windows in v0.4.x and was a constant source of
// "did this state leak across windows?" bugs).
//
// Update-check triggers and focus listeners are wired through
// `lifecycle.ts` so the dialog entry can do the same without duplication.

import { mount, unmount } from "svelte";
import "./app.css";
import "./i18n";
import Settings from "./lib/Settings.svelte";
import { installUpdateChecks } from "./lib/lifecycle";

const target = document.getElementById("app");
if (!target) {
  throw new Error("setup: #app mount point missing in setup.html");
}

const teardownLifecycle = installUpdateChecks();
const app = mount(Settings, { target });

// Hot-module-replacement teardown so dev reloads don't leave dangling
// listeners. In production this branch is dead-eliminated by Vite.
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    teardownLifecycle();
    void unmount(app);
  });
}

export default app;
