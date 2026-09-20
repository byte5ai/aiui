// Pure value logic behind `Form.svelte` (#206).
//
// Everything in here decides *what the user's answer is*: the initial value
// each field starts at, whether a field satisfies its own spec, and what gets
// serialised back to the agent. It used to live inside the component, where
// nothing could execute it — `svelte-check` and `vite build` type it and
// bundle it, neither runs a line of it. Extracted as plain functions taking
// the field spec (and the `values` record) as arguments so `form-values.test.ts`
// can cover it without a DOM.
//
// The component keeps the rendering, the reactive `$state` and the event
// handlers; it owns no copy of the logic below.

export type SelectOption = { label: string; value: string; description?: string };

export type TreeItem = {
  label: string;
  value: string;
  description?: string;
  children?: TreeItem[];
};

export type ListItem = {
  label: string;
  value: string;
  description?: string;
  thumbnail?: string; // data: URL or absolute path
};

export type ImageGridItem = {
  value: string;
  src: string; // data: URL or path
  label?: string;
};

export type TableColumn = {
  key: string;
  label: string;
  align?: "left" | "right" | "center";
};
export type TableRow = {
  value: string;
  values: Record<string, string | number | null>;
};

// Issue #135: optional per-field file-write target. Orthogonal to the field
// kind. On affirmative submit aiui writes the entered value to this path on
// the agent's host; for a `secret` field the value is written-only (never
// returned). The actual write + destination resolution happen Rust-side
// (DialogShell → write_dialog_targets); here it only drives the inline note.
export type WriteTarget = {
  mode: "create" | "substitute";
  path: string;
  perm?: string;
  overwrite?: boolean;
  placeholder?: string;
};

export type Field =
  | { kind: "text"; name: string; label: string; placeholder?: string; default?: string; multiline?: boolean; required?: boolean; target?: WriteTarget }
  | { kind: "password"; name: string; label: string; placeholder?: string; required?: boolean; target?: WriteTarget }
  | { kind: "secret"; name: string; label: string; placeholder?: string; required?: boolean; target?: WriteTarget }
  | { kind: "number"; name: string; label: string; default?: number; min?: number; max?: number; step?: number; required?: boolean; target?: WriteTarget }
  | { kind: "select"; name: string; label: string; options: SelectOption[]; default?: string; required?: boolean }
  | { kind: "checkbox"; name: string; label: string; default?: boolean }
  | { kind: "slider"; name: string; label: string; min: number; max: number; step?: number; default?: number }
  | { kind: "date"; name: string; label: string; default?: string; required?: boolean }
  | { kind: "datetime"; name: string; label: string; default?: string; required?: boolean }
  | { kind: "date_range"; name: string; label: string; default?: { from?: string; to?: string }; required?: boolean }
  | { kind: "color"; name: string; label: string; default?: string }
  | { kind: "static_text"; text: string; tone?: "info" | "warn" | "muted" }
  | { kind: "markdown"; text: string }
  | { kind: "image"; src: string; label?: string; alt?: string; max_height?: number }
  | {
      kind: "annotated_image";
      name: string;
      src: string;
      label?: string;
      alt?: string;
      /** point → single marker, region → rectangle, both → user picks a tool. Default "point". */
      mode?: "point" | "region" | "both";
      max_height?: number;
      required?: boolean;
      default?: {
        point?: { x: number; y: number };
        region?: { x: number; y: number; w: number; h: number };
      };
    }
  | { kind: "audio"; src: string; label?: string }
  | { kind: "mermaid"; source: string; label?: string; max_height?: number }
  | {
      kind: "wireframe";
      panels: Array<{
        title?: string;
        content?: string;
        col_span?: number;
        row_span?: number;
        tone?: "default" | "muted" | "highlight";
      }>;
      columns?: number;
      gap?: number;
      label?: string;
      max_height?: number;
    }
  | {
      kind: "image_grid";
      name: string;
      label?: string;
      images: ImageGridItem[];
      multi_select?: boolean;
      columns?: number;
      default_selected?: string[];
      required?: boolean;
    }
  | {
      kind: "list";
      name: string;
      label?: string;
      items: ListItem[];
      selectable?: boolean;
      multi_select?: boolean;
      sortable?: boolean;
      default_selected?: string[];
    }
  | {
      kind: "table";
      name: string;
      label?: string;
      columns: TableColumn[];
      rows: TableRow[];
      multi_select?: boolean;
      sortable_by_column?: boolean;
      default_selected?: string[];
      required?: boolean;
    }
  | {
      kind: "tree";
      name: string;
      label?: string;
      items: TreeItem[];
      multi_select?: boolean;
      default_selected?: string[];
      default_expanded?: string[];
    };

export type Action = {
  label: string;
  value: string;
  primary?: boolean;
  destructive?: boolean;
  /** Positive-outcome styling (green). For "success"-type semantics like "Approve", "Accept", "Publish". */
  success?: boolean;
  /** If true, field-level required-validation is skipped when this action fires (e.g. "defer"). */
  skip_validation?: boolean;
  /**
   * Issue #177: an action with `skip_validation` is an escape hatch and does
   * NOT commit `target` file writes. Set this to opt such an action back in.
   * The decision is enforced in the writers (Rust `action_commits_targets`,
   * Python `_action_commits_targets`) against the stored spec, not here.
   */
  writes_targets?: boolean;
};

export type Tab = { label: string; fields: Field[] };

export type AnnField = Extract<Field, { kind: "annotated_image" }>;
export type AnnValue = {
  point: { x: number; y: number } | null;
  region: { x: number; y: number; w: number; h: number } | null;
  natural: { width: number; height: number } | null;
};

export type TableValue = {
  selected: string[];
  order: string[];
  sort: { column: string | null; dir: "asc" | "desc" };
};

export type Values = Record<string, any>;

/** Field kinds that carry no value — pure presentation. */
const DISPLAY_KINDS = new Set([
  "static_text",
  "markdown",
  "image",
  "audio",
  "mermaid",
  "wireframe",
]);

export function collectTreeValues(items: TreeItem[]): string[] {
  return items.flatMap((it) => [it.value, ...collectTreeValues(it.children ?? [])]);
}

// Be forgiving when an MCP caller hands us list items as plain strings
// (`items: ["A", "B", "C"]`) instead of the documented
// `items: [{label, value}]` shape. Without this, the agent's first
// attempt at a sortable list often produces an empty render — the
// documented shape isn't easy to discover from the tool's input
// schema. Normalize once and use everywhere.
export function listItems(f: Extract<Field, { kind: "list" }>): ListItem[] {
  return (f.items as unknown as Array<ListItem | string>).map((it) =>
    typeof it === "string" ? { label: it, value: it } : it,
  );
}

// --- default normalisation (#206) -----------------------------------------
// `initialValue` used to hand the agent's `default` straight to the widget.
// Svelte's `bind:value` writes state into the DOM but only reads the
// browser-sanitised result back on a real `input` event — so a default the
// native control cannot represent renders as empty (date/datetime), clamped
// (slider) or black (color) while the *unsanitised* original stays in state
// and is what the agent gets back on submit. We normalise up front instead:
// what the user sees is what the agent receives, and an unrepresentable
// default is cleared rather than silently round-tripped.

function isRealYmd(y: number, m: number, d: number): boolean {
  if (m < 1 || m > 12 || d < 1 || d > 31) return false;
  const probe = new Date(Date.UTC(y, m - 1, d));
  return (
    probe.getUTCFullYear() === y && probe.getUTCMonth() === m - 1 && probe.getUTCDate() === d
  );
}

/**
 * `YYYY-MM-DD` for `<input type="date">`, or `""` when the default cannot be
 * read as an ISO calendar date. Deliberately strict: `Date.parse` accepts
 * locale-ish strings like `"01.06.2026"` with a month/day order that differs
 * per engine, so guessing there would hand the user a date the agent did not
 * mean. An empty field is honest; a wrong date is not.
 */
export function normaliseDate(v: unknown): string {
  if (typeof v !== "string") return "";
  const m = /^(\d{4})-(\d{2})-(\d{2})(?:[T ]|$)/.exec(v.trim());
  if (!m) return "";
  const [, y, mo, d] = m;
  return isRealYmd(Number(y), Number(mo), Number(d)) ? `${y}-${mo}-${d}` : "";
}

/**
 * `YYYY-MM-DDTHH:MM` for `<input type="datetime-local">`, or `""`. Seconds and
 * a trailing `Z` are dropped — the control has no slot for them, and a value
 * it cannot display must not survive into the result.
 */
export function normaliseDateTime(v: unknown): string {
  if (typeof v !== "string") return "";
  const m = /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})/.exec(v.trim());
  if (!m) return "";
  const [, y, mo, d, hh, mm] = m;
  if (!isRealYmd(Number(y), Number(mo), Number(d))) return "";
  if (Number(hh) > 23 || Number(mm) > 59) return "";
  return `${y}-${mo}-${d}T${hh}:${mm}`;
}

/** `#rrggbb` for `<input type="color">`; anything else falls back to black. */
export function normaliseColor(v: unknown): string {
  if (typeof v !== "string") return "#000000";
  const s = v.trim();
  if (/^#[0-9a-f]{6}$/i.test(s)) return s;
  const short = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/i.exec(s);
  if (short) return `#${short[1]}${short[1]}${short[2]}${short[2]}${short[3]}${short[3]}`;
  return "#000000";
}

function clamp(n: number, min?: number, max?: number): number {
  if (min !== undefined && n < min) return min;
  if (max !== undefined && n > max) return max;
  return n;
}

export function initialValue(f: Field): any {
  switch (f.kind) {
    case "static_text":
    case "markdown":
    case "image":
    case "audio":
    case "mermaid":
    case "wireframe":
      return undefined;
    case "checkbox":
      return f.default ?? false;
    case "slider": {
      const raw = f.default ?? f.min;
      const n = Number(raw);
      return Number.isFinite(n) ? clamp(n, f.min, f.max) : f.min;
    }
    case "number": {
      if (f.default === undefined || f.default === null) return "";
      const n = Number(f.default);
      return Number.isFinite(n) ? clamp(n, f.min, f.max) : "";
    }
    case "color":
      return normaliseColor(f.default);
    case "date":
      return normaliseDate(f.default);
    case "datetime":
      return normaliseDateTime(f.default);
    case "date_range":
      return { from: normaliseDate(f.default?.from), to: normaliseDate(f.default?.to) };
    case "select":
      // Svelte's select binding only adopts the DOM selection when the bound
      // value is `undefined`; `""` matches no <option>, so the dropdown used
      // to render blank and submit `""` — a value the agent never offered.
      return f.default ?? f.options?.[0]?.value ?? "";
    case "list":
      return {
        selected: [...(f.default_selected ?? [])],
        order: listItems(f).map((it) => it.value),
      };
    case "table":
      return {
        selected: [...(f.default_selected ?? [])],
        order: f.rows.map((r) => r.value),
        sort: { column: null as string | null, dir: "asc" as "asc" | "desc" },
      };
    case "image_grid":
      return { selected: [...(f.default_selected ?? [])] };
    case "annotated_image":
      // Normalized (0..1) coordinates. `natural` is filled in once the
      // image loads so the agent can recover pixel coordinates losslessly.
      return {
        point: f.default?.point ?? null,
        region: f.default?.region ?? null,
        natural: null as { width: number; height: number } | null,
      };
    case "tree":
      return {
        selected: [...(f.default_selected ?? [])],
        expanded: new Set(f.default_expanded ?? collectTreeValues(f.items)),
      };
    default:
      return (f as any).default ?? "";
  }
}

export function valueFields(fs: Field[]): Field[] {
  return fs.filter((f) => !DISPLAY_KINDS.has(f.kind));
}

export function annHasAnnotation(f: AnnField, values: Values): boolean {
  const v = values[f.name] as AnnValue | undefined;
  if (!v) return false;
  if (f.mode === "point") return !!v.point;
  if (f.mode === "region") return !!v.region;
  // "both" (and the default): either satisfies — presence wins over the
  // currently selected tool.
  return !!v.point || !!v.region;
}

/** Sorted copy of a `table` value — the caller assigns the result back. */
export function sortTableBy(
  field: { name: string; rows: TableRow[] },
  key: string,
  t: TableValue,
): TableValue {
  const dir = t.sort.column === key && t.sort.dir === "asc" ? "desc" : "asc";
  const rowMap = new Map(field.rows.map((r) => [r.value, r]));
  const order = [...t.order].sort((a, b) => {
    const av = rowMap.get(a)?.values[key];
    const bv = rowMap.get(b)?.values[key];
    const cmp =
      av === null || av === undefined
        ? 1
        : bv === null || bv === undefined
        ? -1
        : typeof av === "number" && typeof bv === "number"
        ? av - bv
        : String(av).localeCompare(String(bv));
    return dir === "asc" ? cmp : -cmp;
  });
  return { ...t, order, sort: { column: key, dir } };
}

// --- validation -----------------------------------------------------------

export function isFieldComplete(f: Field, values: Values): boolean {
  if (DISPLAY_KINDS.has(f.kind)) return true;
  if (f.kind === "checkbox") return true;
  if (f.kind === "list" || f.kind === "tree") return true;
  if (f.kind === "slider") {
    // `slider` has no `required` — an untouched field is always complete —
    // but the bounds the agent declared are still bounds.
    const v = values[f.name];
    if (v === undefined || v === null || v === "") return true;
    const n = Number(v);
    if (!Number.isFinite(n)) return false;
    return n >= f.min && n <= f.max;
  }
  if (f.kind === "number") {
    // `min`/`max` on `<input type="number">` only constrain the stepper
    // arrows; a typed-in 9999 in a 1–10 field is accepted by the DOM. This is
    // the only place the declared interval is actually enforced.
    const v = values[f.name];
    if (v === undefined || v === null || v === "") return !f.required;
    const n = Number(v);
    if (!Number.isFinite(n)) return false;
    if (f.min !== undefined && n < f.min) return false;
    if (f.max !== undefined && n > f.max) return false;
    return true;
  }
  if (f.kind === "date_range") {
    // The value is an object, so the generic tail below would stringify it to
    // "[object Object]" and call every range complete.
    if (!f.required) return true;
    const v = values[f.name] as { from?: string; to?: string } | undefined;
    return !!v?.from && !!v?.to;
  }
  if (f.kind === "table" || f.kind === "image_grid") {
    if (!f.required) return true;
    const v = values[f.name] as { selected: string[] } | undefined;
    return !!v && v.selected.length > 0;
  }
  if (f.kind === "annotated_image") {
    if (!f.required) return true;
    return annHasAnnotation(f, values);
  }
  if (!("required" in f) || !f.required) return true;
  const v = values[(f as any).name];
  // Trimmed: "   " is not an answer to a required question.
  return v !== undefined && v !== null && String(v).trim().length > 0;
}

/** Every field that fails its own spec, in declaration order. */
export function incompleteFields(fields: Field[], values: Values): Field[] {
  return fields.filter((f) => !isFieldComplete(f, values));
}

/** Find the first tab (index) containing an incomplete required field, or
 *  null when all tabs validate. Used to surface the first invalid tab in
 *  the validation hint. */
export function firstIncompleteTab(
  tabs: Tab[] | undefined,
  values: Values,
): { tabIndex: number; tabLabel: string } | null {
  if (!tabs) return null;
  for (let i = 0; i < tabs.length; i++) {
    if (!tabs[i].fields.every((f) => isFieldComplete(f, values))) {
      return { tabIndex: i, tabLabel: tabs[i].label };
    }
  }
  return null;
}

/** JSON-safe copy of the value record — a `tree`'s `expanded` Set is UI state
 *  and would serialise to `{}`, so it is dropped rather than shipped. */
export function serialisableValues(values: Values): Record<string, any> {
  const out: Record<string, any> = {};
  for (const [k, v] of Object.entries(values)) {
    if (v && typeof v === "object" && "expanded" in v && v.expanded instanceof Set) {
      out[k] = { selected: v.selected };
    } else {
      out[k] = v;
    }
  }
  return out;
}
