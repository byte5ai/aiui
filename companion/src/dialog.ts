// Entry point for the **dialog** window only. Loaded by `dialog.html`,
// which is the URL Rust passes to `WebviewWindowBuilder::ensure_dialog_window`.
// Mounts `DialogShell.svelte` directly — no runtime label branching.

import { mount, unmount } from "svelte";
import "./app.css";
import "./i18n";
import DialogShell from "./lib/DialogShell.svelte";
import { installUpdateChecks } from "./lib/lifecycle";

const target = document.getElementById("app");
if (!target) {
  throw new Error("dialog: #app mount point missing in dialog.html");
}

const teardownLifecycle = installUpdateChecks();
const app = mount(DialogShell, { target });

if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    teardownLifecycle();
    void unmount(app);
  });
}

export default app;
