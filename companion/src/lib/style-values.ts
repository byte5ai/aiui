// Spec values that end up inside a `style` attribute (review finding D-01).
//
// A widget that writes ``style={`max-height: ${f.max_height}px`}`` hands the
// agent a CSS injection point: nothing on the way here type-checks
// `max_height` or `columns`, so `"1px; position:fixed; inset:0; z-index:9"`
// arrives as a string and becomes declarations of its own. In a window whose
// whole purpose is "the user clicks Confirm", positioned agent CSS is UI
// redressing: lay a picture of swapped buttons over the real ones, or hide
// the file-write approval line.
//
// Every spec value that reaches a style string goes through here and comes
// out as a finite number inside fixed bounds, or not at all. Only the number
// is ever interpolated, never the caller's text.

/** Generous for any dialog, small enough that the value stays a number. */
export const MAX_HEIGHT_PX = 10_000;

/**
 * `v` as a finite number clamped into `[min, max]`, or `undefined` when it is
 * not a number. Numeric strings (`"240"`) are accepted, because a bridge may
 * hand a number over as text; the result is always the parsed number.
 */
export function boundedNumber(v: unknown, min: number, max: number): number | undefined {
  if (typeof v === "string") {
    if (v.trim() === "") return undefined;
  } else if (typeof v !== "number") {
    return undefined;
  }
  const n = Number(v);
  if (!Number.isFinite(n)) return undefined;
  return Math.min(max, Math.max(min, n));
}

/** {@link boundedNumber}, rounded down to an integer (grid tracks, spans). */
export function boundedInt(v: unknown, min: number, max: number): number | undefined {
  const n = boundedNumber(v, min, max);
  return n === undefined ? undefined : Math.max(min, Math.floor(n));
}

/**
 * `max-height: Npx` for a spec's `max_height`, or `""` when there is no
 * usable value. Zero or less means "no cap", as a falsy value always did.
 */
export function maxHeightStyle(v: unknown): string {
  const n = boundedNumber(v, 0, MAX_HEIGHT_PX);
  return n ? `max-height: ${n}px` : "";
}
