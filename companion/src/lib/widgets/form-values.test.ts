// #206: the value logic behind `form` — what a field starts at, whether it
// satisfies the spec the agent sent, and what is serialised back. Every case
// below was a way for the dialog to hand the agent a value its own spec
// forbids.

import { describe, expect, it } from "vitest";
import {
  collectTreeValues,
  firstIncompleteTab,
  formatUtcOffset,
  incompleteFields,
  initialValue,
  inOrder,
  isFieldComplete,
  listItems,
  serialisableValues,
  sliderBounds,
  sortTableBy,
  withLocalOffset,
  type Field,
  type TableValue,
  type Tab,
} from "./form-values";

/** `YYYY-MM-DDTHH:MM` of an instant in the test machine's own zone. */
function localWallClock(iso: string): string {
  const d = new Date(iso);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

describe("isFieldComplete — date_range", () => {
  const f = (required: boolean): Field => ({
    kind: "date_range",
    name: "span",
    label: "Span",
    required,
  });

  it("is incomplete when both ends are empty", () => {
    // The object used to fall through to `String(v).length > 0`, and
    // String({from,to}) is "[object Object]" — always truthy.
    expect(isFieldComplete(f(true), { span: { from: "", to: "" } })).toBe(false);
  });

  it("is incomplete with only one end set", () => {
    expect(isFieldComplete(f(true), { span: { from: "2026-06-01", to: "" } })).toBe(false);
    expect(isFieldComplete(f(true), { span: { from: "", to: "2026-06-30" } })).toBe(false);
  });

  it("is complete with both ends set", () => {
    expect(isFieldComplete(f(true), { span: { from: "2026-06-01", to: "2026-06-30" } })).toBe(
      true,
    );
  });

  it("is complete when not required", () => {
    expect(isFieldComplete(f(false), { span: { from: "", to: "" } })).toBe(true);
  });
});

describe("isFieldComplete — text", () => {
  const f: Field = { kind: "text", name: "why", label: "Why", required: true };

  it("does not accept whitespace as an answer", () => {
    expect(isFieldComplete(f, { why: "   " })).toBe(false);
    expect(isFieldComplete(f, { why: "" })).toBe(false);
  });

  it("accepts padded real input", () => {
    expect(isFieldComplete(f, { why: " x " })).toBe(true);
  });
});

describe("isFieldComplete — number", () => {
  const f = (required = false): Field => ({
    kind: "number",
    name: "replicas",
    label: "Replicas",
    min: 1,
    max: 10,
    required,
  });

  it("rejects a value outside the declared interval", () => {
    // Native min/max only constrain the stepper arrows — 9999 can be typed.
    expect(isFieldComplete(f(), { replicas: 9999 })).toBe(false);
    expect(isFieldComplete(f(), { replicas: 0 })).toBe(false);
  });

  it("accepts a value inside the interval, as number or string", () => {
    expect(isFieldComplete(f(), { replicas: 5 })).toBe(true);
    expect(isFieldComplete(f(), { replicas: "5" })).toBe(true);
  });

  it("treats empty as the required flag says", () => {
    expect(isFieldComplete(f(false), { replicas: "" })).toBe(true);
    expect(isFieldComplete(f(true), { replicas: "" })).toBe(false);
    expect(isFieldComplete(f(true), { replicas: null })).toBe(false);
  });

  it("rejects a non-numeric value", () => {
    expect(isFieldComplete(f(), { replicas: "abc" })).toBe(false);
  });

  it("enforces only the bounds that were declared", () => {
    const open: Field = { kind: "number", name: "n", label: "N" };
    expect(isFieldComplete(open, { n: -9999 })).toBe(true);
  });
});

describe("isFieldComplete — slider", () => {
  const f: Field = { kind: "slider", name: "pct", label: "Percent", min: 0, max: 100 };

  it("enforces bounds even though slider has no required flag", () => {
    expect(isFieldComplete(f, { pct: 200 })).toBe(false);
    expect(isFieldComplete(f, { pct: -1 })).toBe(false);
    expect(isFieldComplete(f, { pct: 50 })).toBe(true);
  });

  it("stays complete for an untouched field", () => {
    expect(isFieldComplete(f, {})).toBe(true);
  });

  // D-09: comparing against an `undefined` bound failed every value, so a
  // slider without `min`/`max` could never be submitted. The control runs
  // 0–100 then, and so does the check.
  it("treats missing bounds as the range input's own 0–100", () => {
    const bare: Field = { kind: "slider", name: "pct", label: "Percent" };
    expect(sliderBounds(bare)).toEqual([0, 100]);
    expect(isFieldComplete(bare, { pct: 50 })).toBe(true);
    expect(isFieldComplete(bare, { pct: 0 })).toBe(true);
    expect(isFieldComplete(bare, { pct: 101 })).toBe(false);
    expect(initialValue(bare)).toBe(0);
    expect(isFieldComplete(bare, { pct: initialValue(bare) })).toBe(true);
  });
});

describe("isFieldComplete — selection widgets", () => {
  it("requires a table selection when required", () => {
    const f: Field = {
      kind: "table",
      name: "rows",
      columns: [{ key: "a", label: "A" }],
      rows: [{ value: "r1", values: { a: "1" } }],
      required: true,
    };
    expect(isFieldComplete(f, { rows: { selected: [], order: ["r1"], sort: {} } })).toBe(false);
    expect(isFieldComplete(f, { rows: { selected: ["r1"], order: ["r1"], sort: {} } })).toBe(true);
  });

  it("requires an annotation when required", () => {
    const f: Field = {
      kind: "annotated_image",
      name: "spot",
      src: "x.png",
      mode: "point",
      required: true,
    };
    expect(isFieldComplete(f, { spot: { point: null, region: null, natural: null } })).toBe(false);
    expect(
      isFieldComplete(f, { spot: { point: { x: 0.1, y: 0.2 }, region: null, natural: null } }),
    ).toBe(true);
  });

  it("ignores display-only fields", () => {
    expect(isFieldComplete({ kind: "static_text", text: "hi" }, {})).toBe(true);
    expect(isFieldComplete({ kind: "markdown", text: "# hi" }, {})).toBe(true);
  });
});

describe("initialValue", () => {
  it("resolves a select without default to the first option", () => {
    // "" matches no <option>, so the dropdown rendered blank and submitted a
    // value the agent never offered.
    const f: Field = {
      kind: "select",
      name: "scope",
      label: "Scope",
      options: [
        { label: "Repo", value: "repo" },
        { label: "Org", value: "org" },
      ],
    };
    expect(initialValue(f)).toBe("repo");
  });

  it("keeps an explicit select default", () => {
    const f: Field = {
      kind: "select",
      name: "scope",
      label: "Scope",
      options: [
        { label: "Repo", value: "repo" },
        { label: "Org", value: "org" },
      ],
      default: "org",
    };
    expect(initialValue(f)).toBe("org");
  });

  it("normalises or clears a date default", () => {
    const f = (d: string): Field => ({ kind: "date", name: "d", label: "D", default: d });
    expect(initialValue(f("2026-06-01T00:00:00Z"))).toBe("2026-06-01");
    expect(initialValue(f("2026-06-01"))).toBe("2026-06-01");
    // Locale-ish input is cleared rather than guessed — never round-tripped.
    expect(initialValue(f("01.06.2026"))).toBe("");
    expect(initialValue(f("tomorrow"))).toBe("");
    expect(initialValue(f("2026-02-31"))).toBe("");
    expect(initialValue({ kind: "date", name: "d", label: "D" })).toBe("");
  });

  it("truncates a wall-clock datetime default to minutes", () => {
    const f = (d: string): Field => ({ kind: "datetime", name: "t", label: "T", default: d });
    expect(initialValue(f("2026-06-01T09:30:45"))).toBe("2026-06-01T09:30");
    expect(initialValue(f("2026-06-01T09:30"))).toBe("2026-06-01T09:30");
    expect(initialValue(f("2026-06-01"))).toBe("");
    expect(initialValue(f("2026-06-01T99:30"))).toBe("");
  });

  // D-13: `Z` / an offset makes the default an instant. Its digits used to be
  // kept and the zone dropped, so 09:30 UTC showed as 09:30 local.
  it("shows a zoned datetime default at the user's local time", () => {
    const f = (d: string): Field => ({ kind: "datetime", name: "t", label: "T", default: d });
    expect(initialValue(f("2026-06-01T09:30:00Z"))).toBe(localWallClock("2026-06-01T09:30:00Z"));
    expect(initialValue(f("2026-06-01T09:30:00+05:30"))).toBe(
      localWallClock("2026-06-01T09:30:00+05:30"),
    );
    // Compact offsets mean the same instant.
    expect(initialValue(f("2026-06-01T09:30+0530"))).toBe(localWallClock("2026-06-01T09:30:00+05:30"));
    expect(initialValue(f("2026-06-01T09:30-03"))).toBe(localWallClock("2026-06-01T09:30:00-03:00"));
  });

  it("normalises both ends of a date_range default", () => {
    const f: Field = {
      kind: "date_range",
      name: "span",
      label: "Span",
      default: { from: "2026-06-01T00:00:00Z", to: "nonsense" },
    };
    expect(initialValue(f)).toEqual({ from: "2026-06-01", to: "" });
    expect(initialValue({ kind: "date_range", name: "s", label: "S" })).toEqual({
      from: "",
      to: "",
    });
  });

  it("clamps an out-of-range slider default", () => {
    const f: Field = { kind: "slider", name: "p", label: "P", min: 0, max: 100, default: 200 };
    expect(initialValue(f)).toBe(100);
    expect(initialValue({ ...f, default: -5 })).toBe(0);
    expect(initialValue({ ...f, default: 42 })).toBe(42);
    // Documented default: the lower bound.
    expect(initialValue({ kind: "slider", name: "p", label: "P", min: 3, max: 9 })).toBe(3);
  });

  it("clamps an out-of-range number default", () => {
    const f: Field = { kind: "number", name: "n", label: "N", min: 1, max: 10, default: 9999 };
    expect(initialValue(f)).toBe(10);
    expect(initialValue({ ...f, default: 0 })).toBe(1);
  });

  // D-14: "no number" was `""` when untouched and `null` once typed and
  // cleared (Svelte's number binding). One shape now: `null`.
  it("starts an empty number at null, the same as a cleared one", () => {
    expect(initialValue({ kind: "number", name: "n", label: "N" })).toBeNull();
    expect(initialValue({ kind: "number", name: "n", label: "N", default: "many" as any })).toBeNull();
  });

  // D-09 (a): a `default` that is not an option rendered a blank dropdown and
  // came back as the user's answer.
  it("falls back to the first option for a select default it does not offer", () => {
    const f: Field = {
      kind: "select",
      name: "env",
      label: "Env",
      options: [
        { label: "Production", value: "production" },
        { label: "Staging", value: "staging" },
      ],
      default: "prod",
    };
    expect(initialValue(f)).toBe("production");
  });

  // D-09 (b): a pre-selection the widget does not render cannot be seen or
  // undone, so it must not become the user's answer.
  it("keeps only default_selected values the widget shows", () => {
    expect(
      initialValue({
        kind: "table",
        name: "t",
        columns: [{ key: "a", label: "A" }],
        rows: [{ value: "r1", values: { a: "1" } }],
        multi_select: true,
        default_selected: ["r1", "row-9"],
      }).selected,
    ).toEqual(["r1"]);
    expect(
      initialValue({
        kind: "image_grid",
        name: "g",
        images: [{ value: "a", src: "x" }],
        default_selected: ["zzz"],
      }).selected,
    ).toEqual([]);
    expect(
      initialValue({
        kind: "tree",
        name: "t",
        items: [{ label: "R", value: "r", children: [{ label: "K", value: "k" }] }],
        multi_select: true,
        default_selected: ["k", "ghost"],
      }).selected,
    ).toEqual(["k"]);
  });

  it("keeps at most one default selection on a single-select widget", () => {
    const rows = [
      { value: "r1", values: {} },
      { value: "r2", values: {} },
    ];
    expect(
      initialValue({ kind: "table", name: "t", columns: [], rows, default_selected: ["r2", "r1"] })
        .selected,
    ).toEqual(["r2"]);
    expect(
      initialValue({
        kind: "list",
        name: "l",
        items: [
          { label: "A", value: "a" },
          { label: "B", value: "b" },
        ],
        selectable: true,
        default_selected: ["a", "b"],
      }).selected,
    ).toEqual(["a"]);
  });

  it("returns no selection for a list that has no checkboxes", () => {
    expect(
      initialValue({
        kind: "list",
        name: "l",
        items: [{ label: "A", value: "a" }],
        sortable: true,
        default_selected: ["a"],
      }).selected,
    ).toEqual([]);
  });

  it("falls back for a colour the native control cannot show", () => {
    const f = (d?: string): Field => ({ kind: "color", name: "c", label: "C", default: d });
    expect(initialValue(f("red"))).toBe("#000000");
    expect(initialValue(f("#AABBCC"))).toBe("#AABBCC");
    expect(initialValue(f("#abc"))).toBe("#aabbcc");
    expect(initialValue(f())).toBe("#000000");
  });

  it("holds the documented per-kind defaults", () => {
    expect(initialValue({ kind: "checkbox", name: "b", label: "B" })).toBe(false);
    expect(initialValue({ kind: "text", name: "t", label: "T" })).toBe("");
    expect(
      initialValue({
        kind: "table",
        name: "rows",
        columns: [{ key: "a", label: "A" }],
        rows: [{ value: "r1", values: { a: "1" } }],
      }),
    ).toEqual({ selected: [], order: ["r1"], sort: { column: null, dir: "asc" } });
    const tree = initialValue({
      kind: "tree",
      name: "t",
      items: [{ label: "Root", value: "root", children: [{ label: "Kid", value: "kid" }] }],
    });
    expect(tree.selected).toEqual([]);
    expect(tree.expanded).toBeInstanceOf(Set);
    expect([...tree.expanded]).toEqual(["root", "kid"]);
    expect(initialValue({ kind: "image", src: "x.png" })).toBeUndefined();
  });
});

describe("serialisableValues", () => {
  it("drops a tree's expanded Set", () => {
    const out = serialisableValues({
      picks: { selected: ["a"], expanded: new Set(["a", "b"]) },
      name: "x",
    });
    expect(out.picks).toEqual({ selected: ["a"] });
    expect("expanded" in out.picks).toBe(false);
    expect(() => JSON.stringify(out)).not.toThrow();
    expect(JSON.parse(JSON.stringify(out))).toEqual({ picks: { selected: ["a"] }, name: "x" });
  });

  // D-13: the control returns wall-clock time with no zone; the agent needs
  // to know whose wall clock.
  it("adds the user's UTC offset to a datetime answer", () => {
    const fields: Field[] = [
      { kind: "datetime", name: "at", label: "At" },
      { kind: "text", name: "note", label: "Note" },
    ];
    const out = serialisableValues({ at: "2026-06-01T09:30", note: "2026-06-01T09:30" }, fields);
    const offset = -new Date(2026, 5, 1, 9, 30).getTimezoneOffset();
    expect(out.at).toBe(`2026-06-01T09:30${formatUtcOffset(offset)}`);
    // It names the instant the user picked.
    expect(new Date(out.at).getTime()).toBe(new Date(2026, 5, 1, 9, 30).getTime());
    // Only datetime fields, and an empty answer stays empty.
    expect(out.note).toBe("2026-06-01T09:30");
    expect(serialisableValues({ at: "" }, fields).at).toBe("");
  });
});

describe("formatUtcOffset / withLocalOffset", () => {
  it("formats offsets east and west of UTC", () => {
    expect(formatUtcOffset(120)).toBe("+02:00");
    expect(formatUtcOffset(330)).toBe("+05:30");
    expect(formatUtcOffset(-300)).toBe("-05:00");
    expect(formatUtcOffset(0)).toBe("+00:00");
    expect(formatUtcOffset(-0)).toBe("+00:00");
  });

  it("leaves anything but a complete wall-clock value alone", () => {
    expect(withLocalOffset("")).toBe("");
    expect(withLocalOffset(null)).toBeNull();
    expect(withLocalOffset("2026-06-01")).toBe("2026-06-01");
  });
});

describe("firstIncompleteTab", () => {
  const tabs: Tab[] = [
    { label: "One", fields: [{ kind: "text", name: "a", label: "A" }] },
    { label: "Two", fields: [{ kind: "text", name: "b", label: "B" }] },
    { label: "Three", fields: [{ kind: "text", name: "c", label: "C", required: true }] },
  ];

  it("points at the tab holding the first incomplete field", () => {
    expect(firstIncompleteTab(tabs, { a: "", b: "", c: "" })).toEqual({
      tabIndex: 2,
      tabLabel: "Three",
    });
  });

  it("is null once everything validates", () => {
    expect(firstIncompleteTab(tabs, { a: "", b: "", c: "done" })).toBeNull();
    expect(firstIncompleteTab(undefined, {})).toBeNull();
  });
});

describe("incompleteFields", () => {
  it("lists every failing field in declaration order", () => {
    const fields: Field[] = [
      { kind: "text", name: "a", label: "A", required: true },
      { kind: "text", name: "b", label: "B" },
      { kind: "number", name: "c", label: "C", min: 1, max: 10 },
    ];
    const bad = incompleteFields(fields, { a: " ", b: "", c: 99 });
    expect(bad.map((f) => (f as any).name)).toEqual(["a", "c"]);
  });
});

describe("listItems", () => {
  it("accepts plain strings as well as {label, value}", () => {
    expect(
      listItems({ kind: "list", name: "l", items: ["A", { label: "B", value: "b" }] as any }),
    ).toEqual([
      { label: "A", value: "A" },
      { label: "B", value: "b" },
    ]);
  });

  // D-05 / A-02: the list is keyed by value; a repeated key blanks the window.
  it("never yields a repeated value", () => {
    const items = listItems({
      kind: "list",
      name: "l",
      items: [{ label: "A" }, { label: "B" }, { label: "A2", value: "x" }, { label: "A3", value: "x" }] as any,
    });
    expect(items.map((i) => i.value)).toEqual(["A", "B", "x"]);
  });
});

describe("inOrder", () => {
  it("hands out each item once, even when values collide or are missing", () => {
    const rows = [{ v: "a" }, { v: "a" }, { v: undefined }, { v: undefined }];
    const out = inOrder(rows, rows.map((r) => r.v), (r) => r.v);
    expect(out).toHaveLength(4);
    expect(new Set(out).size).toBe(4);
  });

  it("follows the given order", () => {
    const rows = [{ v: "a" }, { v: "b" }, { v: "c" }];
    expect(inOrder(rows, ["c", "a", "b"], (r) => r.v).map((r) => r.v)).toEqual(["c", "a", "b"]);
  });
});

describe("collectTreeValues", () => {
  it("walks children depth-first", () => {
    expect(
      collectTreeValues([
        { label: "R", value: "r", children: [{ label: "K", value: "k" }] },
        { label: "S", value: "s" },
      ]),
    ).toEqual(["r", "k", "s"]);
  });
});

describe("sortTableBy", () => {
  const field = {
    name: "rows",
    rows: [
      { value: "r1", values: { n: 3, s: "b" } },
      { value: "r2", values: { n: 1, s: "a" } },
      { value: "r3", values: { n: null, s: "c" } },
    ],
  };
  const t: TableValue = {
    selected: ["r1"],
    order: ["r1", "r2", "r3"],
    sort: { column: null, dir: "asc" },
  };

  it("sorts numerically and pushes empty cells last", () => {
    const out = sortTableBy(field, "n", t);
    expect(out.order).toEqual(["r2", "r1", "r3"]);
    expect(out.sort).toEqual({ column: "n", dir: "asc" });
    expect(out.selected).toEqual(["r1"]);
  });

  it("flips direction on a second click of the same column", () => {
    const asc = sortTableBy(field, "s", t);
    expect(asc.order).toEqual(["r2", "r1", "r3"]);
    expect(sortTableBy(field, "s", asc).sort.dir).toBe("desc");
  });

  it("leaves the input value untouched", () => {
    sortTableBy(field, "n", t);
    expect(t.order).toEqual(["r1", "r2", "r3"]);
  });
});
