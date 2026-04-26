---
name: aiui
description: Before writing a yes/no question, a numbered option list, or a multi-question request into the chat, open a native macOS dialog instead — `confirm` for yes/no (always for delete/force-push/drop/deploy), `ask` for one-of-N with per-option context, `form` for ≥ 2 related inputs / secrets / dates / sliders / sortable lists / table-row triage / image confirm.
---

# aiui — Dialog design for Claude agents

aiui exposes three MCP tools that render native dialogs on the user's Mac:

- `confirm` — irreversible yes/no
- `ask` — single- or multi-choice with descriptions and optional free-text fallback
- `form` — composite window with typed fields and multiple action buttons

## Default to a dialog, not to chat

The user installed aiui because they want the agent to *use* it. If you
catch yourself about to write any of these in chat, stop and use aiui
instead:

- "Would you like me to …?", "Should I proceed?", "Are you sure?" → `confirm`
- "Do you want option A or B?", numbered lists for the user to pick from → `ask`
- "Please tell me the …", "What's the …?" with more than one ask → `form`
- Any step that is **destructive or hard to undo** (delete, drop, force-push,
  rollback, prod deploy) → `confirm` with `destructive: true`, even if the
  user already gave loose approval. The dialog makes the consequence
  explicit and ships the structured answer back, no chat parsing.
- Any step that needs a **secret** for a moment (token, password) →
  `form` with a `password` field, never paste in chat.
- Any step that is a **choice with consequences worth seeing side-by-side**
  ("which deploy strategy?", "which migration path?") → `ask` with
  per-option `description`.
- Any step that wants the user to **rank or sort** items → `form` with a
  sortable `list` field.
- Any step that wants a **date, datetime, range, color, or numeric value
  in a bounded interval** → `form` with the matching field.

## When chat actually wins

Skip the dialog for content the user reads, doesn't answer:

- Status reports, summaries, code snippets, logs, error traces — render
  in chat.
- Single free-text answers where the user would type the same thing into
  a dialog box anyway — just ask in chat.
- Anything where the answer is "go on", and the user is paying attention.

## Tool choice

| Intent | Tool |
|---|---|
| Yes/no, especially destructive | `confirm` |
| 2–6 options, possibly with per-option context | `ask` |
| Multi-field input, multi-action footer | `form` |
| Single free-text answer | just ask in chat |
| More than 8 fields | split into multiple `form` calls; do not cram one dialog |

## Writing labels and copy

- Imperative or noun, ≤ 6 words per label, no punctuation, no emoji.
- Parallel grammar within a dialog. Mixing styles ("Name" / "Please enter
  your age" / "What's your role?") reads as AI slop.
- Defaults a real user would actually pick, not `"enter value here"`.
- `description`/`static_text` only when the label alone is ambiguous —
  avoid redundancy.

## Action buttons (form only)

- Verb-based, concrete. `"Create report"` beats `"OK"`.
- Styling (pick one per button):
  - `primary: true` → blue, the main action.
  - `success: true` → green, positive-outcome verbs ("Approve", "Publish").
  - `destructive: true` → red, irreversible verbs ("Delete", "Rollback").
  - none → neutral outlined button.

  Never style a save button red; never style a delete button green.
- Offer an escape hatch (`skip_validation: true`) so required-field validation
  never traps the user.
- ≤ 3 actions. If you're tempted to add a fourth, rethink the flow.

## The `list` field — one widget, four modes

| `selectable` | `multi_select` | `sortable` | Mode |
|---|---|---|---|
| – | – | – | Static info list |
| ✓ | – | – | Single-choice (radio) |
| ✓ | ✓ | – | Multi-choice (checkboxes) |
| – | – | ✓ | Ordering via drag handles |
| ✓ | ✓ | ✓ | Pick-and-order |

Result is always `{selected: [values], order: [values]}` — `order` reflects
drag changes, `selected` reflects checkbox state. Items can carry a
`thumbnail` (data: URL or path) — perfect for shotlists, mood boards,
carousel slides where the visual anchor matters more than the label.

## The `table` field — column-aware row triage

When you'd otherwise dump 30 branches / 50 search results / 20 stale
files into chat, hand it as a `table` instead. Columns carry the context
(date, size, owner) that `list` can't, rows are clickable for selection,
and the agent gets back the picked rows by their `value`.

```
columns: [{key, label, align?: "left"|"right"|"center"}]
rows:    [{value, values: {<key>: <string|number|null>}}]
multi_select?: true     # checkbox-per-row
sortable_by_column?: true   # click headers to sort
```

Result: `{selected: [values], order: [values], sort: {column, dir}}`. The
order field reflects user-driven sorts so you can preserve their view if
you reopen the form.

## Inline-context fields: `markdown`, `image`, `static_text`

These don't ask anything — they sit between input fields to give context
*for* the inputs that follow.

- `markdown` — rendered Markdown block (lists, code, links, tables). Use
  for "here's the diff I generated, now decide" patterns. **Not** a
  standalone display tool — if you'd be tempted to open a window just to
  show the user a markdown blob, render it in chat instead.
- `image` — read-only single image preview (`src`: data: URL or path,
  optional `label`, `alt`, `max_height`). Use when the agent generated
  a chart, screenshot, or diagram and needs visual sign-off before the
  next decision.
- `static_text` — plain styled note with `tone: "info"|"warn"|"muted"`.
  Lighter weight than `markdown` when no formatting is needed.

## Visual pickers: `image_grid`

For "pick one (or more) of these N generated images" — logo variants,
thumbnail candidates, asset triage. Spec: `images: [{value, src, label?}]`,
`multi_select?`, `columns?` (default 3). Result: `{selected: [values]}`.

## `datetime` field

Lückenfüller between `date` and `date_range`. Cron, scheduling, reminders —
one field instead of splitting into two `text` fields with manual
validation. Native `<input type="datetime-local">`, returns ISO
`YYYY-MM-DDTHH:MM`.

## Tabs — long forms without scroll fatigue

Drop `fields=…` and pass `tabs=[{label, fields: [...]}, ...]` instead.
One submit covers all tabs; validation jumps to the first invalid tab
automatically. Tabs are *display structure*, not a wizard — no per-tab
confirmation, no per-tab actions, all values land in one response.

Use when a single dialog naturally falls into 2-4 distinct topical
groups (e.g. "Identity / Permissions / Notifications" on a user-create
form). Don't reach for tabs to cram a 30-field form into 5 tabs — split
into multiple `form` calls instead.

## Password fields

For short-lived secrets (one-off API tokens, test passwords), prefer
`form` with a `password` field over asking in chat: the value is masked
on screen while the user types, so it doesn't appear in screen
recordings or to a shoulder-surfer.

Be honest with the user, though — the value still returns to you as
plaintext in the tool response. For long-lived or high-value secrets,
tell the user to put them in their keychain or an env var and reference
them by name instead.

## Anti-patterns (slop vs. clean)

| Slop | Clean |
|---|---|
| `confirm(title="Are you sure?")` | `confirm(title="Drop table 'orders'?", destructive=True, message="18,432 rows will be removed.")` |
| `ask(question="Choose one", options=[{"label": "Option 1"}, …])` | `ask(question="Which migration strategy?", options=[{"label":"In-place","description":"Fast, no rollback."}, …])` |
| `form` with 15 `text` fields | Split into logical steps, or push back to chat entirely |
| Button labels "OK" / "Cancel" | "Deploy" / "Discard" — name what happens |
| `static_text` echoing the title | `static_text` adds context the labels can't carry alone |

## Quick-reference example

```python
aiui.form(
    title="New feature draft",
    header="Discovery",
    fields=[
        {"kind": "text", "name": "job", "label": "User job",
         "multiline": True, "required": True},
        {"kind": "select", "name": "scope", "label": "Scope",
         "options": [{"label": "Quick win", "value": "qw"},
                     {"label": "Feature", "value": "f"},
                     {"label": "Epic", "value": "e"}],
         "default": "f"},
        {"kind": "list", "name": "stakeholders", "label": "Stakeholders",
         "items": [{"label": "Product", "value": "prod"},
                   {"label": "Design", "value": "design"},
                   {"label": "Engineering", "value": "eng"}],
         "selectable": True, "multi_select": True,
         "default_selected": ["prod", "eng"]},
        {"kind": "date", "name": "deadline", "label": "Target date"},
    ],
    actions=[
        {"label": "Cancel", "value": "cancel", "skip_validation": True},
        {"label": "Save draft", "value": "draft", "skip_validation": True},
        {"label": "Create", "value": "commit", "primary": True},
    ],
)
```

Response: `{cancelled: false, action: "commit", values: {job: "…",
scope: "f", stakeholders: {selected: [...], order: [...]}, deadline: "…"}}`.
