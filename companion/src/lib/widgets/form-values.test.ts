// #206: the value logic behind `form` — what a field starts at, whether it
// satisfies the spec the agent sent, and what is serialised back. Every case
// below was a way for the dialog to hand the agent a value its own spec
// forbids.

import { describe, expect, it } from "vitest";
import {
  collectTreeValues,
  firstIncompleteTab,
  incompleteFields,
  initialValue,
  isFieldComplete,
  listItems,
  serialisableValues,
  sortTableBy,
  type Field,
  type TableValue,
  type Tab,
} from "./form-values";

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

  it("truncates a datetime default to minutes", () => {
    const f = (d: string): Field => ({ kind: "datetime", name: "t", label: "T", default: d });
    expect(initialValue(f("2026-06-01T09:30:00Z"))).toBe("2026-06-01T09:30");
    expect(initialValue(f("2026-06-01T09:30"))).toBe("2026-06-01T09:30");
    expect(initialValue(f("2026-06-01"))).toBe("");
    expect(initialValue(f("2026-06-01T99:30"))).toBe("");
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
    expect(initialValue({ kind: "number", name: "n", label: "N" })).toBe("");
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
