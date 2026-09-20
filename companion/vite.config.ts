import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { resolve } from "node:path";

// Multi-window setup: Tauri's settings window and the dialog window each
// load their own HTML entry, mount their own Svelte component, and never
// touch each other's code. This replaces the v0.4.x single-bundle model
// where one `App.svelte` branched on `getCurrentWebviewWindow().label`
// at runtime — that pattern leaked state across windows and bloated the
// dialog window with Settings.svelte's tunnel/skill/update machinery.
//
// Each entry pulls in `app.css` and the i18n bootstrap as side-effects;
// Vite hashes the bundles separately so the dialog window doesn't ship
// Settings.svelte's code and vice versa.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        setup: resolve(__dirname, "setup.html"),
        dialog: resolve(__dirname, "dialog.html"),
      },
    },
  },
  // Under Vitest, resolve Svelte through its browser condition — the server
  // build would otherwise be picked and component tests could not mount.
  resolve: process.env.VITEST ? { conditions: ["browser"] } : undefined,
  // #206: the frontend had no test runner at all, so 1300 lines of
  // contract-bearing form logic were covered by nothing but a type-check.
  test: {
    environment: "jsdom",
    include: ["src/**/*.{test,spec}.{js,ts}"],
  },
});
