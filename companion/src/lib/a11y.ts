/**
 * Shared keyboard-activation helpers for the dialog widgets.
 *
 * A `div`/`tr` carrying `role="button"` and `tabindex="0"` takes focus and
 * paints a focus ring, so it *advertises* itself as operable — but a mouse
 * `onclick` alone leaves Enter and Space dead. Every such element in the
 * dialogs routes its key handling through here so the promise the focus ring
 * makes is actually kept (#207).
 */

/** Enter / Space activate, exactly as a native `<button>` would. */
export function onActivate(e: KeyboardEvent, fn: () => void) {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    fn();
  }
}

/**
 * Keyboard reordering for a sortable list: Alt+↑/↓ (or Cmd/Meta+↑/↓ — macOS
 * hands Alt+Arrow to the WebView's own caret movement in some contexts) moves
 * the focused item one slot. `preventDefault` so the dialog does not scroll
 * underneath the reorder.
 *
 * Returns the requested target index, or `null` when the event is not a
 * reorder gesture — the caller decides whether that index is in range.
 */
export function reorderTarget(e: KeyboardEvent, idx: number): number | null {
  if (!e.altKey && !e.metaKey) return null;
  if (e.key === "ArrowUp") {
    e.preventDefault();
    return idx - 1;
  }
  if (e.key === "ArrowDown") {
    e.preventDefault();
    return idx + 1;
  }
  return null;
}
