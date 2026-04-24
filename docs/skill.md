---
name: aiui widgets
description: Render native macOS dialogs on the user's Mac from any Claude Code session — remote or local. Use when the user benefits from structured input (multi-field forms, sortable lists, sliders) more than from chat.
---

# aiui — Dialog design for Claude agents

aiui exposes three MCP tools that render native dialogs on the user's Mac:

- `confirm` — irreversible yes/no
- `ask` — single- or multi-choice with descriptions and optional free-text fallback
- `form` — composite window with typed fields and multiple action buttons

## When to reach for a dialog vs. chat

Prefer chat when the answer fits in one line and the user would type it
anyway. Prefer a dialog when:

- Structured input beats free-form typing (numbers in a range, dates,
  multi-select, ordered lists, secrets).
- You need several related inputs collected in one step.
- The decision is destructive or high-stakes and benefits from a clearly-framed
  confirmation.

Do **not** use a dialog to display information the chat can render just as
well (status reports, tables, code snippets, logs).

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
- Parallel grammar within a dialog. Mix of styles ("Name" / "Bitte geben Sie
  Ihr Alter ein" / "What's your role?") reads as AI slop.
- Defaults that a real user would pick, not `"enter value here"`.
- `description`/`static_text` only when the label alone is ambiguous —
  avoid redundancy.

## Action buttons (form only)

- Verb-based, concrete. `"Bericht erstellen"` beats `"OK"`.
- Destructive → `destructive: true`. Never style a save button red.
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
drag changes, `selected` reflects checkbox state.

## Password fields

For short-lived secrets (one-off API tokens, test passwords), prefer
`form` with a `password` field over asking in chat: the value is masked
on screen while the user types, so it doesn't appear in screen
recordings or shoulder-surfing distance.

Be honest with the user, though — the value still returns to you as
plaintext in the tool response. For long-lived or high-value secrets,
tell the user to put them in their keychain or an env var and reference
them by name instead.

## Anti-patterns (gut vs. schlecht)

| Slop | Clean |
|---|---|
| `confirm(title="Sicher?")` | `confirm(title="Tabelle 'orders' löschen?", destructive=True, message="18.432 Zeilen werden entfernt.")` |
| `ask(question="Wählen", options=[{"label": "Option 1"}, …])` | `ask(question="Welche Strategie für die Migration?", options=[{"label":"In-place","description":"Schnell, kein Rollback."}, …])` |
| `form` mit 15 `text`-Feldern | Aufteilen in logische Schritte oder ganz in Chat verlagern |
| Button-Labels "OK" / "Abbrechen" | "Deploy starten" / "Verwerfen" |
| `static_text` echot den Titel | `static_text` ergänzt Kontext, den die Labels nicht transportieren |

## Quick reference example

```python
aiui.form(
    title="Neuer Feature-Entwurf",
    header="Discovery",
    fields=[
        {"kind": "text", "name": "job", "label": "User-Job",
         "multiline": True, "required": True},
        {"kind": "select", "name": "scope", "label": "Umfang",
         "options": [{"label": "Quick Win", "value": "qw"},
                     {"label": "Feature", "value": "f"},
                     {"label": "Epic", "value": "e"}],
         "default": "f"},
        {"kind": "list", "name": "stakeholders", "label": "Beteiligte",
         "items": [{"label": "Produkt", "value": "prod"},
                   {"label": "Design", "value": "design"},
                   {"label": "Engineering", "value": "eng"}],
         "selectable": True, "multi_select": True,
         "default_selected": ["prod", "eng"]},
        {"kind": "date", "name": "deadline", "label": "Zieldatum"},
    ],
    actions=[
        {"label": "Abbrechen", "value": "cancel", "skip_validation": True},
        {"label": "Entwurf speichern", "value": "draft", "skip_validation": True},
        {"label": "Anlegen", "value": "commit", "primary": True},
    ],
)
```

Response: `{cancelled: false, action: "commit", values: {job: "...",
scope: "f", stakeholders: {selected: [...], order: [...]}, deadline: "..."}}`.
