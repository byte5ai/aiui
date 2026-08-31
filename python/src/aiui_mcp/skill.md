---
name: aiui
description: Before writing a yes/no question, a numbered option list, or a multi-question request into the chat, open a native desktop dialog instead — `confirm` for yes/no (always for delete/force-push/drop/deploy), `ask` for one-of-N with per-option context, `form` for ≥ 2 related inputs / secrets / dates / sliders / sortable lists / table-row triage / image confirm, `notify` for a fire-and-forget completion signal that doesn't block on a reply.
---

# aiui — Dialog design for Claude agents

aiui exposes MCP tools that render native dialogs on the user's machine, plus
one that doesn't wait for the user at all:

- `confirm` — irreversible yes/no
- `ask` — single- or multi-choice with descriptions and optional free-text fallback
- `form` — composite window with typed fields and multiple action buttons
- `gallery` — batch review of images/videos with a per-item verdict
- `compare` — side-by-side A/B (or A/B/C) content compare, pick one
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
| Yes/no on a generated image ("is this OK?") | `confirm` with `image: {src}` |
| 2–6 options, possibly with per-option context | `ask` |
| Pick one of N images ("A or B or C") | `ask` with `thumbnail` per option |
| Multi-field input, multi-action footer | `form` |
| Listen to a TTS sample / voice memo / sound clip before deciding | `form` with an `audio` field |
| Pick one of *many* images (e.g. 12 logo variants) | `form` with `image_grid` |
| Per-item verdict on a *batch* of images/videos ("approve/revise/skip each") | `gallery` |
| Pick one of 2–3 full variants shown side by side (drafts, headlines, before/after) | `compare` |
| Mark *where* on an image (point / region) | `form` with `annotated_image` |
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
OS sound name, omit for silent. On macOS the first call triggers the one-time
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
the user's machine or a remote SSH host it's reaching over the reverse
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

## Mark a point or region on an image: `annotated_image`

When the answer is *spatial* — "where should the logo go?", "which part
do I crop?", "point at the bug" — show the image and let the user mark it
directly instead of describing it in words. A `form` field. Spec:
`{kind: "annotated_image", name, src, label?, alt?, mode?, max_height?, required?, default?}`.

- `src` — same resolution rules as `image` (absolute / `~/` local path on
  your host, `http(s)://` URL, or `data:` URL). See
  [Image sources](#image-sources-src--thumbnail).
- `mode` — `"point"` (default, click one crosshair), `"region"` (drag a
  rectangle), or `"both"` (a Point/Region toggle; both are returned).
- `default` — seed `{point?: {x, y}, region?: {x, y, w, h}}` in normalized
  units to pre-place a marker the user then nudges.
- `required` — submit stays disabled until the user has marked what the
  mode calls for.

Result under the field `name`:
`{point: {x, y} | null, region: {x, y, w, h} | null, natural: {width, height} | null}`.
All coordinates are **normalized 0..1**, resolution-independent; multiply
by `natural` (the image's intrinsic pixel size) to get pixels. Use it
instead of a `select` full of "top-left / bottom-right" options; for
*picking one image out of many* use `image_grid` (this annotates a
**single** image).

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
the user's machine and the dialog streams them back, so a remote clip plays without
hosting it anywhere.

Result: `{cancelled, decisions: {"<value>": {decision, comment?}}}`. Only
touched items appear — an untouched item means "no verdict", not a default.

## File upload: `upload` (user's machine → your host)

The one reversed flow: `upload` pulls a file **from the user's machine into
your session** over the same `:7777` channel (loopback locally, the SSH
reverse-tunnel remotely) — the native replacement for "please `scp` me
that file". Call it whenever the user wants to hand you a local file
("take this file", "here's the screenshot/PDF", `/aiui:upload`). It opens
a **native file picker on the user's machine**; the chosen file is written to
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

## Side-by-side compare: `compare`

A standalone tool (not a `form` field) for an A/B or A/B/C compare: render
2+ full-content **variants** side by side and let the user click ONE to
pick. Use for "which draft is better", "which of these three headlines",
"before vs. after", "GPT vs. Claude's answer" — anywhere the full content,
not a thumbnail, has to be visible to decide.

Spec: `variants: [{value, label?, content?, src?, alt?, detail?, max_height?}]`
(≥ 2), `sync_scroll?`, `columns?` (default `variants.length`, capped at 4).
Each variant needs a stable `value` (returned as `selected`) and at least
one of `content` (Markdown — draft, diff, code) or `src` (image/video,
same resolution rules as everywhere else; videos render with native
controls). A variant may carry both. `label` defaults to A / B / C … by
position; `detail` is a short caption under the pane.

Result: `{cancelled, selected}` — `selected` is the picked variant's
`value`, set only when the user submits. `sync_scroll: true` locks scroll
across panes (reach for it on long text). `max_height` is dialog-wide, not
per-variant: set on any one variant it caps *every* pane so they stay
equal-height. Use `compare` instead of `ask`+`thumbnail` (too small to
actually compare) or `gallery` (independent per-item verdict, not one
either/or pick).

## Starting window size: `size`

`form`, `gallery`, and `compare` take an optional `size` hint — `"s"`,
`"m"`, `"l"` — and aiui picks good local defaults, clamped to the screen.
(Explicit `width`/`height` in logical px override it; rarely needed.) The
hint is a **floor**: the window opens at `max(content-estimate, hint)`, so
it never opens smaller than the content needs, but a sparse dialog can be
told to start roomy. Windows are always resizable — but many users don't
realise that, so opening at a comfortable size is what separates "polished"
from "looks broken". Use `"m"`/`"l"` for forms with
images/tables/wireframes/many fields, or galleries/compares with heavy
content; leave unset for short forms.

## Image sources (`src` / `thumbnail`)

aiui takes an image source in `confirm` (`image: {src}`), `ask`
(`options[].thumbnail`), the `form` fields `image` / `image_grid` /
`annotated_image` / `list` (`items[].thumbnail`), and the `gallery` /
`compare` tools (`items[].src` / `variants[].src`, images or videos). The
`audio` field takes the same three formats too, with one twist: a local
audio path never inlines as `data:`, it always routes through the
size-unbounded `/media` cache (see [Audio playback](#audio-playback-audio)).

Three input formats render correctly:

- **Local filesystem path** (`/home/me/foo.png`, `~/renders/x.jpg`) — the
  natural choice when the file is already on disk. This bridge reads the
  file and inlines it as a `data:` URL before the spec leaves your host.
  **The path must exist on the host *you*, the agent, run on** — for an
  SSH-tunneled session that's the remote, not the user's machine. Absolute or
  `~/`-rooted only; relative paths are not resolved (no stable `cwd`
  contract on MCP bridges). 10 MB cap.
- **`http(s)://` URL** — fetched on the user's machine and inlined (5 s
  timeout, 10 MB cap, parallel for grids). Use when the image already
  lives on a reachable server; the user's machine contacts the URL, aiui never phones
  home.
- **`data:` URL** (`data:image/png;base64,…`) — the fallback when neither
  path nor URL fits (e.g. bytes generated in-memory). Embed the encoded
  bytes directly in the tool-call `src` — never roundtrip through a shell
  pipeline (see below). Over ~2 MB it starts to feel laggy in the MCP
  transport.

**Pick the simplest one that works:** path if the file's on disk, then URL
if reachable, `data:` only as last resort.

Known footguns: **relative paths** (`./foo.png`, `../x.png` — resolved
against an undefined `cwd`; use absolute / `~/`); **cross-host paths** (a
file on the user's machine won't resolve from a remote agent, or vice versa —
the bridge that reads it is on the agent's host; use `http(s)://` or inline
`data:`); **bare URLs in `markdown` field text** (`![alt](url)` follows the
same CSP — the resolver only walks `src` / `thumbnail`, not markdown
bodies); **`<a href="https://…">` links** work as a click target but open
in the user's default browser, not an image-rendering path. A missing file,
a CSP block, and a 404 all look identical to the user — if they report a
broken image, ask once whether anything appeared at all.

### Anti-pattern: shell-encoding `data:` URLs

Don't write the encoded bytes to a tempfile then `cat` / `printf` them back
through bash to construct the JSON tool call. Two failure modes seen in the
wild: the terminal recognises the `data:image/...` prefix in stdout and
tries to render it inline (eating the rest of the pipeline), and the
encoded payload spans multiple shell-line buffers and gets word-split or
quoting-mangled. The fix is structural — the tool call is JSON, not shell:
build the spec in your runtime and pass
`src=f"data:image/png;base64,{b64}"` straight into the call, or hand aiui
the path and let the bridge do the encoding.

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
