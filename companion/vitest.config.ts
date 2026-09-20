import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Separate from `vite.config.ts` because the app build is a two-entry
// multi-window bundle (setup.html + dialog.html) and the test run is
// neither — sharing one file would mean a `test` block the app build has
// to carry and a `rollupOptions.input` the test run has to ignore.
//
// Added with #208: two of that issue's four defects live entirely in
// `Settings.svelte` (a status poll that outlived the window it belonged
// to, and a health banner that named the wrong cause), and the companion
// had no way to hold frontend behaviour still. `cargo test` covers the
// Rust half; this covers the half that renders.
export default defineConfig({
  plugins: [svelte()],
  // `@testing-library/svelte` mounts components client-side, so Svelte 5
  // must resolve to its browser entry rather than the SSR one.
  resolve: { conditions: ["browser"] },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
    restoreMocks: true,
  },
});
