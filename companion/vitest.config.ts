import { defineConfig } from "vitest/config";

// Kept apart from `vite.config.ts` on purpose: that config is the two-window
// bundle description (setup.html + dialog.html entries, Svelte plugin), none
// of which a unit test needs. What the tests do need is a DOM — `mermaid`
// renders by measuring real elements — so the environment is jsdom.
//
// Issue #212: these tests are the standing answer to "does a dependency bump
// still sanitise what it renders", so they run in CI next to `npm audit`.
export default defineConfig({
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
});
