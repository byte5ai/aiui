// `svelte-i18n` falls back to `en` for a key missing from `de`, so a
// forgotten German string is invisible until a German-locale user hits
// that exact screen. #208 added five keys across two catalogues; this
// keeps the two in step from here on.

import { describe, expect, it } from "vitest";
import de from "./de.json";
import en from "./en.json";

function keys(obj: unknown, prefix = ""): string[] {
  if (typeof obj !== "object" || obj === null) return [prefix];
  return Object.entries(obj as Record<string, unknown>).flatMap(([k, v]) =>
    keys(v, prefix ? `${prefix}.${k}` : k),
  );
}

describe("the i18n catalogues", () => {
  it("carry exactly the same key set", () => {
    const enKeys = keys(en).sort();
    const deKeys = keys(de).sort();
    expect(deKeys.filter((k) => !enKeys.includes(k))).toEqual([]);
    expect(enKeys.filter((k) => !deKeys.includes(k))).toEqual([]);
  });

  it("carry the hints #208 added, so no failure claims a port conflict it cannot prove", () => {
    for (const catalogue of [en, de]) {
      const settings = catalogue.settings as Record<string, string>;
      expect(settings["http_error.hint.token"]).toBeTruthy();
      expect(settings["http_error.hint.unreachable"]).toBeTruthy();
      expect(settings["remotes.remove.confirm"]).toContain("{host}");
      expect(settings["remotes.remove.back"]).toBeTruthy();
      expect(settings["remotes.remove.do"]).toBeTruthy();
      expect(settings["welcome.demo.failed"]).toBeTruthy();
      expect(settings["welcome.demo.manual"]).toBeTruthy();
    }
  });
});
