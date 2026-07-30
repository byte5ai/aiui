---
name: aiui
description: Before writing a yes/no question, a numbered option list, or a multi-question request into the chat, open a native macOS dialog instead — `confirm` for yes/no (always for delete/force-push/drop/deploy), `ask` for one-of-N with per-option context, `form` for ≥ 2 related inputs / secrets / dates / sliders / sortable lists / table-row triage / image confirm, `notify` for a fire-and-forget completion signal that doesn't block on a reply.
---

# aiui — Dialog design for Claude agents

aiui exposes MCP tools that render native dialogs on the user's Mac, plus
one that doesn't wait for the user at all:

- `confirm` — irreversible yes/no
- `ask` — single- or multi-choice with descriptions and optional free-text fallback
- `form` — composite window with typed fields and multiple action buttons
- `gallery` — batch review of images/videos with a per-item verdict
- `notify` — fire-and-forget native OS notification; no dialog, no
  response, returns immediately

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
- Any step that wants the user to **listen to a TTS sample, voice memo,
  or generated sound clip** before confirming, choosing, or triaging it
  → `form` with an `audio` field (native `<audio controls>`). Don't
  paste a file path in chat and ask the user to open it themselves.
- Any **async-completion signal** the user doesn't need to answer — "tests
  are green", "deploy finished", "hit a merge conflict, need you" — where
  the point is exactly that they don't have to be watching this session
  → `notify`, not a chat message and not `confirm`.

## When chat actually wins

Skip the dialog for content the user reads, doesn't answer:

- Status reports, summaries, code snippets, logs, error traces — render
  in chat.
- Single free-text answers where the user would type the same thing into
  a dialog box anyway — just ask in chat.
- Anything where the answer is "go on", and the user is paying attention
  (otherwise see `notify` above).

## Tool choice

| Intent | Tool |
|---|---|
| Yes/no, especially destructive | `confirm` |
| 2–6 options, possibly with per-option context | `ask` |
| Multi-field input, multi-action footer | `form` |
| Listen to a TTS sample / voice memo / sound clip before deciding | `form` with an `audio` field |
| Per-item verdict on a *batch* of images/videos ("approve/revise/skip each") | `gallery` |
| Async-completion signal, no reply needed, user may not be watching | `notify` |
| Single free-text answer | just ask in chat |
| More than 8 fields | split into multiple `form` calls; do not cram one dialog |

## Fire-and-forget: `notify`

`notify` does not open a window and does not wait for the user. Call it,
get `{ok: true}` back immediately. Use it for the class of thing you'd
otherwise announce in chat and hope the user notices — "tests green",
"deploy finished", "merge conflict, need you" — when they may not be
looking at this session at all. That's also what separates it from
`confirm`: if the message expects a reply, use `confirm`/`ask`/`form`
instead — `notify` has no way to carry an answer back.

Spec: `{title, body, subtitle?, sound?}` — `title`/`body` required.
`title` is a short headline (banners truncate past ~40 chars); `body`
carries the detail; `subtitle` is optional extra context (folded into
`body` where there's no distinct subtitle slot); `sound` is an optional
OS sound name, omit for silent. First call triggers the one-time macOS
notification-permission prompt; a denied permission comes back as
`{ok: false, error}`, not a tool error.

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

## Audio playback: `audio`

Read-only, like `image` — sits between input fields for the user to
listen to before deciding. Spec: `{kind: "audio", src, label?}`. Renders
a native `<audio controls>` player; no value is returned in the form
result. `src` accepts a `data:audio/...` URL, an `http(s)://` URL, or an
absolute/`~/` local path (mp3/m4a/wav/aac/ogg/flac). A local audio path
is never inlined as `data:` — even a small clip — it's always pushed
through the same size-unbounded `/media` cache the `gallery` tool uses
for local video, so it works identically whether this bridge runs on
the user's Mac or a remote SSH host it's reaching over the reverse
tunnel.

Use `audio` for "does this TTS voice sound right?", "confirm this
generated jingle", "here's the voice memo — does it transcribe
correctly?".

## Schematic diagrams: `mermaid`

For graph-shaped visualisations — flowcharts, sequence diagrams, state
machines, gantt, ER, class diagrams, mind-maps — use the `mermaid`
field instead of ASCII boxes-and-arrows. Spec:
`{kind: "mermaid", source: "<DSL>", label?, max_height?}`. The `source`
is a Mermaid-DSL string; aiui pipes it through `mermaid.render()`,
sanitises the SVG, and embeds inline. Read-only, sits between input
fields like `markdown` / `image`.

## UI-layout mockups: `wireframe`

Orthogonal to `mermaid` — `mermaid` is for graphs, `wireframe` is for
*layouts*. When you'd otherwise sketch a UI mockup with ASCII
boxes-and-pipes (dashboard tiles, hardware-UI panels, login-screen
sketch, app-surface layout), use `wireframe` instead. Real CSS-Grid
panels, no monospace approximation.

Spec:
`{kind: "wireframe", panels: [{title?, content?, col_span?, row_span?, tone?}], columns?, gap?, label?, max_height?}`.

Each panel has optional `title` (uppercase header), `content`
(multi-line monospace body, escape `\n` for line-breaks), `col_span` /
`row_span` (default 1), and `tone` ∈ `{"default","muted","highlight"}`.

```json
{
  "kind": "wireframe",
  "label": "U-Boot-Funkbude",
  "columns": 3,
  "panels": [
    {"title": "EMPFANG", "col_span": 2, "content": "14:32:07 [SCHWACH] …\n14:32:11 [STARK]   WX MIDWAY"},
    {"title": "STATUS",  "col_span": 1, "content": "Tiefe: 18 m\nKurs:  270°"},
    {"title": "AKTION",  "col_span": 3, "content": "[T]auchen [A]uftauchen [K]urs", "tone": "highlight"}
  ]
}
```

Read-only, sits between input fields. Anti-pattern: ASCII
boxes-and-pipes for *anything* layout-shaped — that is exactly what
this field replaces.

## Visual pickers: `image_grid`

For "pick one (or more) of these N generated images" — logo variants,
thumbnail candidates, asset triage. Spec: `images: [{value, src, label?}]`,
`multi_select?`, `columns?` (default 3). Result: `{selected: [values]}`.

`image_grid` *picks* among candidates. For a **separate verdict per item**
(approve this, revise that, skip the third, optional note each) use the
`gallery` tool below instead.

## Batch review: `gallery`

A standalone tool (not a `form` field) for reviewing a *batch* of images
and/or videos and collecting one decision per item in a single window —
instead of firing `confirm` once per asset.

Spec: `items: [{value, src?, label?, detail?, max_height?}]`,
`actions?` (per-item buttons, default Approve / Revise / Skip),
`comment?` (free-text per item), `columns?`. Each item's `value` must be
non-empty and unique — it keys the result. `src` follows the standard
image rules; **videos** (`data:video/` URL, `http(s)://` URL, or a local
`.mp4`/`.mov`/`.m4v`/`.webm` path) render with native controls. Local
videos of any size work — the bridge pushes them to aiui's media cache on
the Mac and the dialog streams them back, so a remote clip plays without
hosting it anywhere.

Result: `{cancelled, decisions: {"<value>": {decision, comment?}}}`. Only
touched items appear — an untouched item means "no verdict", not a default.

## File upload: `upload` (Mac → your host)

The one reversed flow: `upload` pulls a file **from the user's Mac into
your session** over the same `:7777` channel (loopback locally, the SSH
reverse-tunnel remotely) — the native replacement for "please `scp` me
that file". Call it whenever the user wants to hand you a local file
("take this file", "here's the screenshot/PDF", `/aiui:upload`). It opens
a **native file picker on the Mac**; the chosen file is written to
`target_dir/<filename>` on **your** host.

- Pass `target_dir` (absolute or `~/`-rooted, on your host) inferred from
  context — usually your cwd/project dir. Omit only with no context
  (defaults to cwd). Relative paths rejected; dir must exist and be writable.
- Don't ask which file or where — the user picks in the dialog, you infer
  the target. Deterministic: lands at exactly `target_dir/<filename>`, no
  staging path. Existing files are never overwritten (a clash errors).
- Returns `{status:"ok", path, filename, bytes}` or `{status:"error", error}`
  (cancelled picker, unreadable file, >512 MB cap, missing/unwritable dir).
  Blocks until pick/cancel; progress fires every ~10 s meanwhile.

## Starting window size: `size`

`form` and `gallery` take an optional `size` hint — `"s"`, `"m"`, `"l"` —
and aiui picks good local defaults, clamped to the screen. (Explicit
`width`/`height` in logical px override it; rarely needed.) The hint is a
**floor**: the window opens at `max(content-estimate, hint)`, so it never
opens smaller than the content needs, but a sparse dialog can be told to
start roomy. Windows are always resizable — but many users don't realise
that, so opening at a comfortable size is what separates "polished" from
"looks broken". Use `"m"`/`"l"` for forms with images/tables/wireframes/many
fields, or galleries with a large batch; leave unset for short forms.

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
use the `secret` field with a `target` (below) so the value never enters
the conversation.

## Secrets & file-write: `secret` field + `target` (#135)

When a value must NOT pass through this conversation — a credential the
user pastes that should land in a file, not your transcript — use a
`secret` field with a `target`. Any input field may carry `target`; for a
`secret` field the value is **write-only** (result: `{written, target,
bytes}`, never the value).

```json
{ "kind": "secret", "name": "pat", "label": "GitHub PAT für byte5ai",
  "target": { "mode": "create", "path": "~/.github_tokens/byte5ai",
              "perm": "0600", "overwrite": true } }
```

- `mode:"create"` — write raw value (needs `overwrite:true` to clobber).
- `mode:"substitute"` — replace a `placeholder` occurring exactly once in
  an existing file (YAML/TOML/INI/env); 0 or >1 → error (never misapplied).
  Pick a **distinctive sentinel** (`__AIUI_SECRET_GITHUB_PAT__`, not a common
  word) so the single match is unambiguous, not just lucky.
- Destination is always your own host: the aiui module there (native app
  locally, bridge on a remote SSH host) writes it as a LOCAL file op, so
  `create` and `substitute` both work identically local and remote — no
  foreign host. The user sees the path and approves by submitting. Errors:
  `{written:false, error}`.

Replaces the fragile "guess a shell one-liner to stash a token" pattern.
QoL + confused-deputy guard, not a hard guarantee.

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
