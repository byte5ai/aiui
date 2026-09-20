// #197: the app auto-detects its locale and ships a complete `en.json`, yet
// the whole update surface was hardcoded German — in *native OS modals*,
// the most prominent text the product shows. These two assertions are what
// stops that drifting back.

import { describe, expect, it } from "vitest";

import de from "./de.json";
import en from "./en.json";
// Vite's `?raw` rather than `node:fs`, so the check needs no @types/node and
// runs identically under `vitest` and any future browser-mode runner.
import updaterSource from "../lib/updater.ts?raw";

/** The JSON files nest one level ("app", "settings", "dialog") and then use
 *  dotted keys inside, with one further nesting for `dialog.ttl`. Flatten
 *  everything so parity is a plain set comparison. */
function flatten(obj: unknown, prefix = ""): string[] {
  if (typeof obj !== "object" || obj === null) return [prefix];
  return Object.entries(obj as Record<string, unknown>).flatMap(([k, v]) =>
    flatten(v, prefix ? `${prefix}.${k}` : k),
  );
}

describe("translation catalogues", () => {
  it("has exact de/en key parity", () => {
    const deKeys = new Set(flatten(de));
    const enKeys = new Set(flatten(en));

    const missingInEn = [...deKeys].filter((k) => !enKeys.has(k)).sort();
    const missingInDe = [...enKeys].filter((k) => !deKeys.has(k)).sort();

    expect(missingInEn).toEqual([]);
    expect(missingInDe).toEqual([]);
    expect(deKeys.size).toBeGreaterThanOrEqual(86);
  });
});

/** Strip `//` and block comments so the check is about *strings the user
 *  sees*, not about em dashes in prose written for the next contributor.
 *  Crude on purpose — `updater.ts` has no `//` inside a string literal, and
 *  a false positive here is a failing test, not a shipped bug. */
function stripComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/^\s*\/\/.*$/gm, "");
}

describe("updater.ts", () => {
  it("contains no hardcoded non-ASCII UI text", () => {
    const offenders = stripComments(updaterSource)
      .split("\n")
      .filter((line) => /[^\u0000-\u007F]/.test(line))
      .map((line) => line.trim());

    expect(offenders).toEqual([]);
  });
});
