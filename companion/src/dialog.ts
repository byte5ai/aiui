// Entry point for the **dialog** window only. Loaded by `dialog.html`,
// which is the URL Rust passes to `WebviewWindowBuilder::ensure_dialog_window`.
// Mounts `DialogShell.svelte` directly — no runtime label branching.
//
// Deliberately does NOT install the update-check lifecycle (#195). This window
// renders content an agent on a remote host authored, so it is the last place
// that should hold the updater; `capabilities/dialog.json` grants it neither
// `updater:*` nor `process:*`, and the headless 6 h check in `run()` (lib.rs)
// already covers detection for the whole app. See `lib/lifecycle.ts`.

import { mount, unmount } from "svelte";
import "./app.css";
import "./i18n";
import DialogShell from "./lib/DialogShell.svelte";

const target = document.getElementById("app");
if (!target) {
  throw new Error("dialog: #app mount point missing in dialog.html");
}

const app = mount(DialogShell, { target });

if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    void unmount(app);
  });
}

export default app;
