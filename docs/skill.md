---
name: aiui
description: Render native desktop dialogs on the user's machine via aiui's MCP server — `confirm` before destructive actions (delete, drop, force-push, deploy), `ask` for pick-one-of-N where context per option matters, `form` for multi-input requests, secrets, dates, sliders, sortable lists, or image confirmation, `compare` for A/B(/C) side-by-side picks, `gallery` for batch image/video review, `notify` for a fire-and-forget completion signal that doesn't block on a reply.
---

# aiui — Dialog design for Claude agents

aiui exposes MCP tools that render native dialogs on the user's machine,
plus one that doesn't wait for the user at all:

- `confirm` — irreversible yes/no
- `ask` — single- or multi-choice with descriptions and optional free-text fallback
- `form` — composite window with typed fields and multiple action buttons
- `gallery` — batch review of images/videos, one decision per item
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
- Any step where you'd sketch a **flow, sequence, state, hierarchy or
  schedule in ASCII** ("Step A → Step B → ...") → `form` with a
  `mermaid` field. ASCII boxes-and-arrows look terrible in any
  proportional-font surface; the `mermaid` field renders to clean
  SVG. See the dedicated section below.
- Any step that asks **"is this generated image OK?"** → `confirm`
  with `image: {src}`. Don't fall back to a `form`-with-image-and-two-
  buttons when the question is a plain yes/no.
- Any step that wants the user to **listen to a TTS sample, voice memo,
  or generated sound clip** before confirming, choosing, or triaging it
  → `form` with an `audio` field (native `<audio controls>`). Don't
  paste a file path in chat and ask the user to open it themselves.
- Any step that asks **"which of these images?"** with 2–6 candidates
  → `ask` with `thumbnail` per option. Use `form` + `image_grid` only
  when there are many candidates (≥ 7) or the picker needs multi-select.
- Any step where the user needs to see **full content side by side**
  before choosing one — two drafts, three headlines, before/after an
  edit — → `compare`. Don't reach for `ask`+thumbnail here: a thumbnail
  is too small to actually compare, `compare` renders the full pane.
- Any **async-completion signal** the user doesn't need to answer — "tests
  are green", "deploy finished", "hit a merge conflict, need you" — where
  the point is exactly that they don't have to be watching this session
  → `notify`, not a chat message and not `confirm`. If you catch yourself
  about to end a turn with nothing but "done!" while the user has tabbed
  away, that's a `notify`, not silence.

## When chat actually wins

Skip the dialog for content the user reads, doesn't answer:

- Status reports, summaries, code snippets, logs, error traces — render
  in chat.
- Single free-text answers where the user would type the same thing into
  a dialog box anyway — just ask in chat.
- Anything where the answer is "go on", and the user is paying attention
  (i.e. they're actually looking at this session — otherwise see `notify`
  above).

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

`notify` is the odd one out: it does not open a window and does not wait
for the user. Call it, get `{ok: true}` back immediately, move on. Use it
for the class of thing you'd otherwise announce in chat and hope the user
notices — "tests green", "deploy finished", "hit a merge conflict, need
you" — when they may not be looking at this session at all. That's also
the line that separates it from `confirm`: if the message expects a reply,
it's the wrong tool — `notify` has no way to carry an answer back.

Spec: `{title, body, subtitle?, sound?}`. `title` and `body` are required.

- `title` — short headline, notification banners truncate anything past
  ~40 characters. State the outcome, not the process ("Deploy finished",
  not "Deploying...").
- `body` — the detail: what finished, what needs attention, what broke.
- `subtitle` — optional extra context line (folded into `body` on
  platforms/backends without a distinct subtitle slot — don't rely on it
  rendering as a visually separate line).
- `sound` — optional OS sound name (e.g. `"default"`); omit for a silent
  notification.

On macOS the first call triggers the one-time notification-permission prompt,
same as any other native app — if the user has denied it, `notify`
returns `{ok: false, error}` rather than erroring the tool call; that's
an expected outcome, not a bug to retry around.

**Anti-patterns:**

- Using `notify` for anything that expects an answer — that's `confirm`/
  `ask`/`form`. `notify` is one-way.
- Firing `notify` for routine intermediate progress ("step 3 of 10") —
  reserve it for the completion/attention-worthy moment, or every step
  becomes a notification and the signal drowns.
- Padding `title` with a full sentence — put the sentence in `body`.

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
- UI-layout mockups (dashboard tiles, hardware panels, screen
  layouts) — Mermaid is graph-DSL, not layout-DSL. Use `wireframe`
  for that, see below.

## UI-layout mockups: `wireframe`

When you'd otherwise reach for ASCII boxes-and-pipes to mock a UI
layout — dashboard tiles, hardware-UI panels, a login-screen sketch,
a hand-drawn-feel app surface — **stop**. Use the `wireframe` field
in a `form` instead. `mermaid` covers diagrams (graphs, flows, state);
`wireframe` covers fixed-position panel grids.

Spec:
`{kind: "wireframe", panels: [{title?, content?, col_span?, row_span?, tone?}], columns?, gap?, label?, max_height?}`.

`panels` is the only required field. Each panel has:

- `title` — uppercase header, optional
- `content` — multi-line monospace body, escape `\n` for line-breaks
- `col_span` / `row_span` — default 1
- `tone` — `"default"` (neutral), `"muted"` (de-emphasised),
  `"highlight"` (accent border + tinted background)

aiui renders real CSS-Grid panels with proper borders, monospace
content, and theme-matched colours — the layout actually looks like
the layout, instead of approximating it with `+`s and `|`s.

```
{
  "kind": "wireframe",
  "label": "U-Boot-Funkbude",
  "columns": 3,
  "panels": [
    {"title": "EMPFANG", "col_span": 2, "content": "14:32:07 [SCHWACH] …\n14:32:11 [STARK]   WX MIDWAY"},
    {"title": "STATUS",  "col_span": 1, "content": "Tiefe: 18 m\nKurs:  270°\nSpeed: 8 kn"},
    {"title": "SIGNAL",  "col_span": 1, "content": "Atmo:    █▒▒\nWetter:  ▓▒▒", "tone": "muted"},
    {"title": "AKTION",  "col_span": 2, "content": "[T]auchen [A]uftauchen [K]urs", "tone": "highlight"}
  ]
}
```

Read-only — sits between input fields to give layout context, like
`markdown` / `image` / `mermaid`. Result-handling unchanged.

**Anti-patterns:**

- ASCII boxes-and-pipes for *anything* layout-shaped — that's the
  slop this field replaces.
- Using `wireframe` for graphs / flows / sequences — those are
  `mermaid`-shaped, the panels here have no edges.
- Putting deeply nested layouts inside one panel's `content` — the
  field is for the *outer* layout. If you need nested boxes, split
  into more panels with `col_span` / `row_span`.
- Free-form HTML / markdown inside `content` — content is plain
  text, rendered monospace; everything else is intentionally ignored.

## Mark a point or region on an image: `annotated_image`

When the answer you need is *spatial* — "where should the logo go?", "which
part do I crop?", "point at the bug in this screenshot" — words are a poor
carrier. Show the image and let the user mark it directly.

Spec:
`{kind: "annotated_image", name, src, label?, alt?, mode?, max_height?, required?, default?}`.

- `src` — same resolution rules as `image` (absolute / `~/` local path,
  `http(s)://` URL, or `data:` URL). See [Image sources](#image-sources-src--thumbnail).
- `mode` — what the user can mark:
  - `"point"` (default) — click to drop a single crosshair marker.
  - `"region"` — drag to draw a rectangle.
  - `"both"` — a Point/Region toggle appears; the user can set a point
    *and* a region (both are returned).
- `default` — optionally seed `{point?: {x, y}, region?: {x, y, w, h}}` in
  normalized units to pre-place a marker the user then nudges.
- `required` — the submit action stays disabled until the user has marked
  the annotation the mode calls for.

**Result** (under the field `name`):

```
{
  "point":  {"x": 0.42, "y": 0.31} | null,
  "region": {"x": 0.10, "y": 0.20, "w": 0.30, "h": 0.25} | null,
  "natural": {"width": 1920, "height": 1080} | null
}
```

All coordinates are **normalized 0..1** relative to the image — resolution
independent, so they survive the image being displayed at any size. `region`
is top-left `x,y` plus `w,h`. Multiply by `natural` (the image's intrinsic
pixel size, filled in once it loads) to get pixel coordinates:
`px = point.x * natural.width`.

```
{
  "kind": "annotated_image",
  "name": "logo_spot",
  "label": "Where should the logo sit?",
  "src": "~/renders/hero.png",
  "mode": "point"
}
```

**Anti-patterns:**

- Asking "top-left or bottom-right?" in a `select` when the honest answer
  is a spot on the image — that's exactly what this field is for.
- Using it for *picking one image out of many* — that's `image_grid` (this
  field annotates a **single** image).
- Expecting pixel coordinates in the result without reading `natural` — the
  raw `x/y/w/h` are fractions, not pixels.

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

## Audio playback: `audio`

Read-only, like `image` — sits between input fields for the user to
listen to before deciding. Spec: `{kind: "audio", src, label?}`. Renders
a native `<audio controls>` player; no value is returned in the form
result.

```json
{ "kind": "audio", "src": "~/Downloads/tts-sample.mp3", "label": "Voice: warm" }
```

`src` follows the **same resolution rules as `image`** (see
[Image sources](#image-sources-src--thumbnail) below), plus one twist:
a **local** audio path (`mp3`/`m4a`/`wav`/`aac`/`ogg`/`flac`) is never
inlined as a `data:` URL, no matter how small. It's pushed through the
same size-unbounded `/media` cache the `gallery` tool uses for local
video — the bridge uploads the bytes to the user's machine and the dialog streams
them back over a loopback URL, working identically whether you run
locally or on a remote SSH host. `http(s)://` and `data:` audio URLs
work exactly like they do for `image` — no special handling needed.

Use `audio` for "does this TTS voice sound right?", "confirm this
generated jingle", "here's the voice memo the user attached — does it
transcribe correctly?". For a plain yes/no on the clip, still reach for
`confirm` if there's nothing else to fill in; use `audio` inside a
`form` when the listen-then-decide step needs other inputs alongside it
(a text field for feedback, a `select` for which voice to use next,
etc.).

## Visual pickers: `image_grid`

For "pick one (or more) of these N generated images" — logo variants,
thumbnail candidates, asset triage. Spec: `images: [{value, src, label?}]`,
`multi_select?`, `columns?` (default 3). Result: `{selected: [values]}`.
Each `src` follows the same rules as `image` — see below.

`image_grid` is a *picker* — one (or N) selected out of many. When you
instead need a **separate verdict per item** — approve this, revise that,
skip the third, with an optional note each — use the `gallery` tool below.

## Batch review: `gallery`

A standalone tool (not a `form` field), for reviewing a *batch* of images
and/or videos and collecting one decision per item in a single window —
instead of firing `confirm` once per asset.

Spec: `items: [{value, src?, label?, detail?, max_height?}]`,
`actions?` (per-item buttons, default Approve / Revise / Skip),
`comment?` (free-text field per item), `columns?` (default responsive).
Each item's `value` must be non-empty and unique — it keys the result.
`src` follows the same resolution rules as `image`; **videos** (a
`data:video/` URL, an `http(s)://` URL, or a local `.mp4`/`.mov`/`.m4v`/
`.webm` path) render with native `<video controls>`. Local video files of
any size work: the bridge pushes them to aiui's media cache on the user's machine and
the dialog streams them back (range-seekable), so a remote agent's clip
plays without you hosting it anywhere. `http(s)://` video URLs stream
directly.

Result: `{cancelled, decisions: {"<item value>": {decision, comment?}}}`.
Only items the user actually touched appear in `decisions` — an untouched
item means "no verdict", not a default.

Use `gallery` for "review these 6 hero renders", "triage this screenshot
batch". Use `confirm`+`image` for a single yes/no sign-off, and
`ask`+`thumbnail` / `image_grid` when the task is *picking* among
candidates rather than judging each one.

## File upload: `upload` (user's machine → your host)

Every other aiui data flow goes *your host → the user's machine* (dialog specs down,
image bytes inlined). `upload` is the one that reverses it: it pulls a
file **from the user's machine into your session**, over the same
authenticated `:7777` channel the dialogs use (loopback locally, the SSH
reverse-tunnel remotely). It's the native replacement for "please `scp`
me that file".

Call `upload` whenever the user wants to hand you a local file — "take
this file", "here's the screenshot/PDF/CSV", or the `/aiui:upload`
slash-command. It opens a **native file picker on the user's machine**; the file the
user chooses is streamed back and written to `target_dir/<filename>` on
**your** host (the remote for an SSH session).

- **Pass `target_dir`** — an absolute or `~/`-rooted directory on your
  host, chosen from context (usually your cwd or the active project dir).
  Omit it only when you genuinely have no context (defaults to your
  process's cwd). Relative paths are rejected; the directory must already
  exist and be writable.
- **Don't ask which file or where** — the user picks the file in the
  native dialog, and you infer the target. One tool call, no back-and-forth.
- **Deterministic destination:** the filename comes from the selection,
  so the file lands at exactly `target_dir/<filename>` — no temp/staging
  path. Existing files are **never overwritten**; a name clash returns an
  error instead of clobbering (pick another `target_dir` or move the old
  file first).
- **Result:** `{status: "ok", path, filename, bytes}` on success, or
  `{status: "error", error}` for a cancelled picker, an unreadable file, a
  file over the 512 MB cap, or a missing/unwritable target dir. Report it
  briefly; on `ok`, name the path the file landed at.

Blocks until the user picks or dismisses the picker, exactly like the
dialog tools — progress notifications fire every ~10 s while you wait, so
a slow response just means the user is browsing, not that aiui broke.

## Side-by-side compare: `compare`

A standalone tool (not a `form` field), for an A/B or A/B/C compare:
render 2 or more full-content **variants** next to each other and let
the user click ONE to pick. Use it for "which draft is better", "which
of these three headlines", "before vs. after this edit", "GPT vs.
Claude's answer" — anywhere the full content, not a thumbnail, needs to
be visible to decide.

Spec: `variants: [{value, label?, content?, src?, alt?, detail?,
max_height?}]` (≥ 2 entries), `sync_scroll?`, `columns?` (default
`variants.length`, capped at 4). Each variant needs a stable `value`
(the key returned as `selected`) and at least one of:

- `content` — Markdown text: a draft, a diff, a code snippet.
- `src` — an image or video, same resolution rules as everywhere else
  in aiui (data:, http(s)://, or absolute/`~/` local path). Videos
  render with native controls.

A variant may carry both (an image plus a caption). `label` defaults
to A / B / C / … by position if omitted. `detail` is a short caption
line under the pane (source, score, timestamp).

```json
{
  "title": "Which opening line?",
  "variants": [
    {"value": "a", "label": "Direct", "content": "Your invoice is 12 days overdue."},
    {"value": "b", "label": "Soft",   "content": "Just a friendly nudge about invoice #4471."}
  ]
}
```

Result: `{cancelled, selected}` — `selected` is the `value` of the
picked variant, present only when the user actually submits (Cancel/
Escape leaves it absent, same as everywhere else in aiui).

**`sync_scroll: true`** locks scroll position across all panes — reach
for it when comparing long text so the user can scroll once and see
matching passages line up. Leave it off for short copy or images.

**`max_height` is dialog-wide, not per-variant**: set it on any one
variant and it caps *every* pane's height, because unequal pane heights
break the "side by side" framing. Omit it and aiui picks a sensible
default that grows a little for image/video variants.

Use `compare` instead of `ask`+`thumbnail` (thumbnails are too small to
actually compare) or `gallery` (per-item batch review with an
independent verdict per asset, not a single either/or pick).

## Starting window size: `size` / `width` / `height`

`form`, `gallery`, and `compare` accept an optional **`size`** hint — `"s"`, `"m"`, or
`"l"` — and aiui picks good local defaults for each, clamped to the user's
screen. (Power users can pass explicit `width` / `height` in logical px,
which override `size`; rarely needed.)

The hint is a **floor, not a cap**: the window opens at
`max(content-estimate, hint)`. So a content-heavy dialog never opens
smaller than it needs (you can't cram a 12-image gallery with `size:"s"`),
but a *sparse* dialog you know will feel cramped at the default can be told
to start roomy. Windows are always resizable regardless — but many users
don't realise that, so a dialog that opens at a comfortable size is the
difference between "looks polished" and "looks broken". Reach for `"m"` or
`"l"` when a form carries images, tables, wireframes, or many fields, or a
gallery has a large batch / tall thumbnails. Leave it unset for ordinary
short forms — the auto-estimate already fits those.

## Image sources (`src` / `thumbnail`)

aiui takes an image source in these places:

- `confirm` → `image: {src, alt?, max_height?}` — visual yes/no
- `ask` → `options[].thumbnail` — visual pick-one-of-N
- `form` → `image` field → `src`
- `form` → `image_grid` → `images[].src`
- `form` → `list` → `items[].thumbnail`
- `gallery` → `items[].src` — batch review, images or videos
- `compare` → `variants[].src` — side-by-side pick, images or videos

(`form` → `audio` field → `src` accepts the same three formats too —
see [Audio playback](#audio-playback-audio) above — with one difference:
a local path never inlines as `data:`, it always routes through the
`/media` cache, size-unbounded.)

In all of them the same three input formats render correctly:

- **Local filesystem path** (`/Users/me/foo.png`, `~/Pictures/x.jpg`)
  — *the natural choice when the file is already on disk*. The aiui
  bridge running on your host reads the file and inlines it as a
  `data:` URL before the dialog spec leaves your host. **Important:**
  the path must exist on the host *you*, the agent, are running on —
  for an SSH-tunneled session that's the remote, not the user's machine.
  Absolute or `~/`-rooted paths only — relative paths are not
  resolved (no stable `cwd` contract on MCP bridges). 10 MB cap.
- **`http(s)://` URL** — aiui fetches it on the user's machine and inlines
  it. 5-second timeout, 10 MB cap, parallel fetch for grids. Use when
  the image already lives on a reachable web server. The user's machine contacts
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
- **Cross-host paths.** A path that exists on the user's machine but not
  on the remote where the agent runs (or vice versa) won't resolve —
  the bridge that does the reading is on the agent's host. If you
  need to render a file on the user's machine from a remote agent, use `http(s)://`
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
use the `secret` field with a `target` instead (below) so the value
never enters the conversation.

## Secrets & file-write: the `secret` field + `target` (#135)

When a value must NOT pass through this conversation — a credential the
user pastes that should land in a file, not your transcript — use a
`secret` field with a `target`. Any input field may carry `target`; for a
`secret` field the value is **write-only**: aiui writes it to the file and
returns only `{written, target, bytes}`, never the value.

```json
{ "kind": "secret", "name": "pat", "label": "GitHub PAT für byte5ai",
  "target": { "mode": "create", "path": "~/.github_tokens/byte5ai",
              "perm": "0600", "overwrite": true } }
```

- **`mode: "create"`** — write the raw value. Needs `overwrite: true` to
  replace an existing file (a path typo otherwise fails loudly rather than
  clobbering).
- **`mode: "substitute"`** — replace a `placeholder` that occurs *exactly
  once* in an existing file (format-agnostic: YAML/TOML/INI/env). 0 or >1
  matches → error, never a partial or wrong write. **Pick a distinctive
  sentinel** that cannot collide with real file content — e.g.
  `__AIUI_SECRET_GITHUB_PAT__`, never a common word like `TOKEN` or `X`. The
  exactly-once rule is the safety net (a colliding placeholder errors instead
  of being misapplied), but a distinctive sentinel makes the match
  unambiguous in the first place.
- **Destination is always your own host** — an aiui module already runs
  there (the native app for a local session, the bridge on a remote SSH
  session), and it performs the write as a plain **local** file operation.
  So `create` and `substitute` behave identically local and remote (the
  entered value reaches that module over aiui's own channel, never via the
  agent). You cannot target a foreign host; the user sees the resolved path
  and approves it by submitting.
- **Errors** come back as `{written:false, error}` — no silent success.

Why it exists: it replaces the fragile "guess a shell one-liner to stash a
token" pattern with a native dialog + a correct, atomic write whose target
the user sees first. It's a QoL + confused-deputy guard, **not** a hard
guarantee the agent can't read the value some other way — for that, the
user still types it themselves outside any agent path.

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
