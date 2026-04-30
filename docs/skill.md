---
name: aiui
description: Render native desktop dialogs on the user's machine via aiui's MCP server — `confirm` before destructive actions (delete, drop, force-push, deploy), `ask` for pick-one-of-N where context per option matters, `form` for multi-input requests, secrets, dates, sliders, sortable lists, or image confirmation.
---

# aiui — Dialog design for Claude agents

aiui exposes three MCP tools that render native dialogs on the user's machine:

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
- Any step where you'd sketch a **flow, sequence, state, hierarchy or
  schedule in ASCII** ("Step A → Step B → ...") → `form` with a
  `mermaid` field. ASCII boxes-and-arrows look terrible in any
  proportional-font surface; the `mermaid` field renders to clean
  SVG. See the dedicated section below.
- Any step that asks **"is this generated image OK?"** → `confirm`
  with `image: {src}`. Don't fall back to a `form`-with-image-and-two-
  buttons when the question is a plain yes/no.
- Any step that asks **"which of these images?"** with 2–6 candidates
  → `ask` with `thumbnail` per option. Use `form` + `image_grid` only
  when there are many candidates (≥ 7) or the picker needs multi-select.

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
| Yes/no on a generated image ("is this OK?") | `confirm` with `image: {src}` |
| 2–6 options, possibly with per-option context | `ask` |
| Pick one of N images ("A or B or C") | `ask` with `thumbnail` per option |
| Multi-field input, multi-action footer | `form` |
| Pick one of *many* images (e.g. 12 logo variants) | `form` with `image_grid` |
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
`thumbnail` — see [Image sources](#image-sources-src--thumbnail) below
for the accepted URL formats. Perfect for shotlists, mood boards,
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

## Schematic diagrams: `mermaid`

When you'd otherwise reach for ASCII boxes-and-arrows, draw flowcharts
in `+--+`-style art, or sketch a sequence diagram with `-->` and `|`,
**stop**. Use the `mermaid` field in a `form` instead.

Spec: `{kind: "mermaid", source: "<DSL>", label?: string, max_height?: number}`.

The `source` is a Mermaid-DSL string. aiui pipes it through `mermaid.render()`,
DOMPurify-sanitises the resulting SVG, and embeds it inline. Covers
flowcharts, sequence diagrams, state diagrams, class diagrams, gantt,
ER, mind-maps, and pie charts — pick the one that fits the situation.

```
{
  "kind": "mermaid",
  "source": "graph TD; Start --> Probe; Probe -- ok --> Render; Probe -- fail --> Retry; Retry --> Probe"
}
```

Read-only — like `markdown` and `image`, it sits between input fields
to give context, not to ask anything. Result-handling unchanged.

**Anti-patterns:**

- ASCII / box-drawing art for any flow, sequence, state, or
  hierarchy — that's exactly the slop this field replaces.
- Trying to render a *picture* as Mermaid — Mermaid is structured
  diagrams (nodes, edges, swimlanes), not free drawing. For arbitrary
  images use `image` with a real source.
- Embedding HTML in node labels — Mermaid's `securityLevel: strict`
  rejects it (which we want). Keep labels plain text.

## Inline-context fields: `markdown`, `image`, `static_text`

These don't ask anything — they sit between input fields to give context
*for* the inputs that follow.

- `markdown` — rendered Markdown block (lists, code, links, tables). Use
  for "here's the diff I generated, now decide" patterns. **Not** a
  standalone display tool — if you'd be tempted to open a window just to
  show the user a markdown blob, render it in chat instead.
- `image` — read-only single image preview. `src` accepts a `data:` URL
  or any `http(s)://` URL — see [Image sources](#image-sources-src--thumbnail)
  below. Optional `label`, `alt`, `max_height`. Use when the agent
  generated a chart, screenshot, or diagram and needs visual sign-off
  before the next decision.
- `static_text` — plain styled note with `tone: "info"|"warn"|"muted"`.
  Lighter weight than `markdown` when no formatting is needed.

## Visual pickers: `image_grid`

For "pick one (or more) of these N generated images" — logo variants,
thumbnail candidates, asset triage. Spec: `images: [{value, src, label?}]`,
`multi_select?`, `columns?` (default 3). Result: `{selected: [values]}`.
Each `src` follows the same rules as `image` — see below.

## Image sources (`src` / `thumbnail`)

aiui takes an image source in five places:

- `confirm` → `image: {src, alt?, max_height?}` — visual yes/no
- `ask` → `options[].thumbnail` — visual pick-one-of-N
- `form` → `image` field → `src`
- `form` → `image_grid` → `images[].src`
- `form` → `list` → `items[].thumbnail`

In all of them the same three input formats render correctly:

- **Local filesystem path** (`/Users/me/foo.png`, `~/Pictures/x.jpg`)
  — *the natural choice when the file is already on disk*. The aiui
  bridge running on your host reads the file and inlines it as a
  `data:` URL before the dialog spec leaves your host. **Important:**
  the path must exist on the host *you*, the agent, are running on —
  for an SSH-tunneled session that's the remote, not the user's Mac.
  Absolute or `~/`-rooted paths only — relative paths are not
  resolved (no stable `cwd` contract on MCP bridges). 10 MB cap.
- **`http(s)://` URL** — aiui fetches it on the user's Mac and inlines
  it. 5-second timeout, 10 MB cap, parallel fetch for grids. Use when
  the image already lives on a reachable web server. The Mac contacts
  the URL, not aiui's infrastructure (aiui itself never phones home).
- **`data:` URL** — `data:image/png;base64,…`. The fallback when
  neither path nor URL works (e.g. you generated bytes in-memory and
  don't want to write a tempfile). Embed the encoded bytes directly
  in the tool-call's `src` value — never roundtrip through a shell
  pipeline (see anti-pattern below). Watch the size — over ~2 MB it
  starts to feel laggy in the MCP transport.

**Pick the simplest one that works:** path first if the file's on
disk, then URL if it's reachable, `data:` only as last resort.

What does **not** work — known footguns:

- **Relative paths** (`./foo.png`, `foo.png`, `../assets/x.png`).
  Resolved against an undefined `cwd`. Use absolute or `~/` paths.
- **Cross-host paths.** A path that exists on the user's Mac but not
  on the remote where the agent runs (or vice versa) won't resolve —
  the bridge that does the reading is on the agent's host. If you
  need to render a Mac-side file from a remote agent, use `http(s)://`
  or pass the bytes inline as `data:`.
- **Bare URLs in `markdown` field text.** Markdown's `![alt](url)`
  follows the same CSP — the URL has to resolve to `data:` somehow.
  The resolver only walks `src` / `thumbnail` properties, not the
  bodies of markdown blocks.
- **Linking out** with `<a href="https://...">` from `markdown` —
  works as a click target, but opens in the user's default browser
  (we explicitly intercept it). It's not an image-rendering question.

If you tried a path or URL and the user reports a broken image, ask
them once whether anything appeared at all — a missing file, a CSP
block, and a 404 all look identical to the user. The companion logs
the failure (`imageresolve: …`) but agents can't read those logs.

### Anti-pattern: shell-encoding `data:` URLs

Don't write the encoded bytes to a tempfile, then `cat` or `printf` them
back through bash to construct the JSON tool call. Two failure modes
seen in the wild:

1. The terminal recognises the `data:image/...` prefix in stdout and
   tries to render it inline — eats the rest of the pipeline.
2. The encoded payload spans multiple shell-line buffers and gets
   word-split or quoting-mangled.

The fix is structural: the tool call is JSON, not shell. Either build
the spec dict in your runtime and pass `src=f"data:image/png;base64,{b64}"`
straight into the tool call, or hand aiui the path and let the bridge
do the encoding for you.

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
| `image(src="./shot.png")` (relative path — undefined `cwd`) | `image(src="/Users/me/shot.png")` — absolute, the bridge reads it locally |
| Writing base64 to a tempfile, `cat`-ing it through bash to build the tool call | Pass the path as `src` and let the bridge encode, or build the `data:` URL directly in your runtime — never via shell pipes |

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
