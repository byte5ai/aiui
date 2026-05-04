"""aiui MCP server — renders native macOS dialogs via the aiui companion.

Topology:

    Claude Code (local or remote) ──stdio──► aiui-mcp (this process)
                                               │ HTTP
                                               ▼
                                     http://127.0.0.1:7777
                                               │  (local, or via SSH reverse-tunnel)
                                               ▼
                                       Mac: aiui.app (Tauri companion)

The aiui token is read from `~/.config/aiui/token` — installed once when
the companion runs on the Mac, and scp'd automatically to each remote host
registered in the companion's settings window.
"""
from __future__ import annotations

import base64
import importlib.metadata
import importlib.resources as resources
import logging
import mimetypes
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import httpx
from mcp.server.fastmcp import FastMCP


def _version() -> str:
    try:
        return importlib.metadata.version("aiui-mcp")
    except importlib.metadata.PackageNotFoundError:
        return "dev"


VERSION = _version()
BUILD_INFO = f"aiui-mcp v{VERSION}"

logging.basicConfig(
    level=os.environ.get("AIUI_LOG_LEVEL", "INFO").upper(),
    format="%(asctime)s.%(msecs)03d %(levelname)s %(name)s %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
    stream=sys.stderr,
)
log = logging.getLogger("aiui")
log.info("---- %s started pid=%d ----", BUILD_INFO, os.getpid())


def _default_token_path() -> str:
    """Per-OS default location of the companion's pairing token.

    The companion (Tauri side) writes the token to its OS-correct config
    directory; this server reads from the matching path so the two sides
    agree without needing AIUI_TOKEN_PATH to be set.

    - Linux / macOS: `~/.config/aiui/token` — XDG-style. macOS keeps the
      same path as Linux so existing v0.4.x installs don't have to migrate.
    - Windows: `%APPDATA%\\aiui\\token` — matches Tauri's `dirs::config_dir()`.

    In practice the Python side runs on the Linux remote almost always —
    the Windows branch only kicks in if someone runs `aiui-mcp` directly
    on a Windows host (rare, but no longer broken).
    """
    if sys.platform == "win32":
        appdata = os.environ.get("APPDATA")
        if appdata:
            return str(Path(appdata) / "aiui" / "token")
        # Fallback if APPDATA is somehow unset — unusual on Windows.
        return str(Path.home() / "AppData" / "Roaming" / "aiui" / "token")
    return "~/.config/aiui/token"


TOKEN_PATH = Path(os.environ.get("AIUI_TOKEN_PATH", _default_token_path())).expanduser()
ENDPOINT = os.environ.get("AIUI_ENDPOINT", "http://127.0.0.1:7777")
TIMEOUT_S = float(os.environ.get("AIUI_TIMEOUT_S", "120"))
HEALTH_TIMEOUT_S = float(os.environ.get("AIUI_HEALTH_TIMEOUT_S", "3"))

_INSTRUCTIONS = """\
aiui is connected — you can render native dialogs on the user's Mac \
instead of asking via chat. Default behaviour for this session:

- Yes/no question (esp. before delete / drop / force-push / deploy) → \
  call `confirm` instead of asking in chat.
- Pick-one-of-N options where context per option matters → call `ask`.
- Multiple related inputs, secret, date, slider, sortable order, \
  table-row triage, image confirm/grid → call `form`.
- Pure information the user only reads → keep it in chat.

Type `/aiui:teach` for the full widget catalog when composing a \
complex form.
"""

# `instructions` is the spec-sanctioned way to push a top-level hint
# into every session at the MCP handshake — Claude Code (and Claude
# Desktop) feed it to the agent before the first turn.  Kept short on
# purpose; the full catalog lives in the `widgets`/`teach` prompts.
mcp = FastMCP("aiui", instructions=_INSTRUCTIONS)


def _token() -> str:
    if not TOKEN_PATH.exists():
        raise RuntimeError(
            f"aiui token not found at {TOKEN_PATH}. "
            "Install the aiui companion on your Mac and register this remote from its "
            "settings window (adds the token automatically). "
            "Download: https://github.com/byte5ai/aiui/releases/latest"
        )
    return TOKEN_PATH.read_text().strip()


def _explain_exc(e: BaseException) -> str:
    """Return a non-empty, human-readable description of an exception.

    httpx wraps low-level transport errors (RemoteProtocolError after
    the peer crashed mid-response, ReadError on stream close, …) where
    ``str(e)`` is empty. Without this fallback those surfaced as
    ``error: ""`` in tool responses — useless for diagnosis. The
    exception class name is always present, so it gives the user
    *something* concrete even when httpx provides no message.
    """
    msg = str(e).strip()
    return msg if msg else type(e).__name__


async def _preflight() -> None:
    """Quick sanity check before every render call: the service on :7777 must
    accept our bearer token. Guards against stale local aiui instances that
    would otherwise hijack the SSH reverse-forward and hang dialogs silently.
    """
    async with httpx.AsyncClient(timeout=HEALTH_TIMEOUT_S) as client:
        try:
            r = await client.get(
                f"{ENDPOINT}/health",
                headers={"Authorization": f"Bearer {_token()}"},
            )
        except httpx.ConnectError as e:
            raise RuntimeError(
                f"aiui companion not reachable at {ENDPOINT}. "
                f"Is Claude Desktop running on your Mac? For remote projects, the "
                f"SSH reverse-tunnel must also be active (companion handles it "
                f"automatically if this host is registered in its settings). "
                f"Underlying error: {e}"
            ) from e
        except httpx.ReadTimeout as e:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} timed out on /health — likely a stale "
                f"local aiui instance holding the port. Run `pkill -f '^aiui$'` on "
                f"this host. ({_explain_exc(e)})"
            ) from e
        except httpx.RemoteProtocolError as e:
            # Connection reset / closed mid-response. The on-Mac mcp-stdio
            # child's auto-resurrect normally brings aiui.app back on the
            # next tool call, so a one-off reset is usually self-healing —
            # we name the most common stuck-state causes (stale SSH tunnel
            # squatting :7777, token mismatch from a parallel install)
            # rather than telling the user to manually restart aiui.app.
            # httpx leaves str(e) empty for this class of error — the
            # `_explain_exc` fallback surfaces the class name so the user
            # at least sees *something* concrete.
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} reset the connection. "
                f"The Mac-side mcp-stdio normally auto-resurrects aiui.app on "
                f"the next call — if this persists, a stale process may hold "
                f"the port. Verify that Claude Desktop is open on the Mac and, "
                f"on remotes, re-register the host in aiui.app settings to "
                f"re-sync the token. "
                f"({_explain_exc(e)})"
            ) from e
        except httpx.HTTPError as e:
            # Catch-all for the rest of the httpx hierarchy (RequestError,
            # WriteError, HTTPStatusError, …) so we never bubble up a bare
            # exception with an empty message.
            raise RuntimeError(
                f"aiui companion request to {ENDPOINT} failed: {_explain_exc(e)}. "
                f"Verify Claude Desktop is open on the Mac; auto-resurrect "
                f"normally restores the GUI on the next call. If repeated, "
                f"check the SSH reverse-tunnel and re-register this remote "
                f"in aiui.app settings to re-sync the token."
            ) from e

        if r.status_code == 401:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} rejected our token (401). "
                f"Another aiui process may be listening on this port with a different "
                f"token. Run `pkill -f '^aiui$'` on this host, then re-register it "
                f"from the companion's settings window to re-sync the token."
            )
        if r.status_code != 200:
            raise RuntimeError(
                f"aiui companion /health returned {r.status_code}: {r.text[:200]}"
            )


_SRC_KEYS = {"src", "thumbnail"}
_MAX_IMAGE_BYTES = 10 * 1024 * 1024  # 10 MB — mirrors the Rust resolver
_LOCAL_PATH_MIME_OVERRIDES = {
    # mimetypes.guess_type returns None for SVG without a hint on some
    # Pythons, and `image/svg` (without `+xml`) on others. Lock it down
    # so the WebView always sees the canonical `image/svg+xml`.
    ".svg": "image/svg+xml",
}


def _looks_like_local_path(s: str) -> bool:
    """Mirror of `imageresolve::looks_like_local_path` in the Rust bridge.

    Accepts absolute paths and `~/`-rooted paths. Rejects `data:` URLs,
    `http(s)://` URLs, relative paths (no stable cwd contract on MCP
    bridges), and anything else.
    """
    if not s:
        return False
    if s.startswith(("data:", "http://", "https://")):
        return False
    return s.startswith("/") or s.startswith("~")


def _read_path_as_data_url(raw: str) -> str:
    """Read a local file and return it as `data:<mime>;base64,…`.

    Raises ValueError on anything that should make the resolver leave
    the original `src` value alone (missing file, oversize, not a file).
    """
    path = Path(raw).expanduser()
    if not path.is_file():
        raise ValueError(f"not a file: {path}")
    size = path.stat().st_size
    if size > _MAX_IMAGE_BYTES:
        raise ValueError(f"too large: {size} bytes (max {_MAX_IMAGE_BYTES})")
    ext = path.suffix.lower()
    mime = _LOCAL_PATH_MIME_OVERRIDES.get(ext)
    if mime is None:
        mime, _ = mimetypes.guess_type(str(path))
    if mime is None:
        mime = "application/octet-stream"
    data = path.read_bytes()
    b64 = base64.b64encode(data).decode("ascii")
    return f"data:{mime};base64,{b64}"


def _resolve_local_paths(node: Any) -> None:
    """Walk a render spec in place, replacing absolute / `~/` paths in
    `src` / `thumbnail` properties with `data:` URLs. The bridge-side
    counterpart to the Mac's HTTPS resolver — runs wherever this MCP
    server runs (local or remote), which is by definition the host
    that holds the agent's files.

    Fail-soft: read errors are logged, the original value is kept (the
    WebView will eventually show a broken image rather than the call
    blowing up).
    """
    if isinstance(node, dict):
        for key, value in list(node.items()):
            if key in _SRC_KEYS and isinstance(value, str) and _looks_like_local_path(value):
                try:
                    node[key] = _read_path_as_data_url(value)
                except (OSError, ValueError) as e:
                    log.warning("local path skipped for %s: %s", value, e)
            else:
                _resolve_local_paths(value)
    elif isinstance(node, list):
        for item in node:
            _resolve_local_paths(item)


async def _post_render(spec: dict[str, Any]) -> dict[str, Any]:
    await _preflight()
    t0 = datetime.now(timezone.utc)
    log.info("render → kind=%s", spec.get("kind"))
    # Resolve any absolute / `~/`-rooted file paths *before* shipping
    # the spec down the HTTP wire. This bridge runs on the same host
    # as the agent — local for Mac use, remote for SSH-tunneled
    # remotes — so this is the only point in the chain where the
    # agent's filesystem actually exists. The Mac-side server resolver
    # only handles HTTPS.
    _resolve_local_paths(spec)
    async with httpx.AsyncClient(timeout=TIMEOUT_S) as client:
        r = await client.post(
            f"{ENDPOINT}/render",
            headers={"Authorization": f"Bearer {_token()}"},
            json={"spec": spec},
        )
        r.raise_for_status()
    dt = (datetime.now(timezone.utc) - t0).total_seconds()
    data = r.json()
    log.info(
        "render ← kind=%s cancelled=%s took=%.2fs",
        spec.get("kind"), data.get("cancelled"), dt,
    )
    return data


def _format_result(payload: dict[str, Any]) -> dict[str, Any]:
    if payload.get("cancelled"):
        return {"cancelled": True}
    return {"cancelled": False, **payload.get("result", {})}


@mcp.tool()
async def ask(
    question: str,
    options: list[dict[str, Any]],
    header: str | None = None,
    multi_select: bool = False,
    allow_other: bool = True,
) -> dict[str, Any]:
    """Before listing options in chat and waiting for the user to type back
    which one (deploy strategy, migration path, file to act on …), call
    this tool instead. Per-option `description` carries the trade-off;
    `multi_select` and `allow_other` cover the rest.

    WHEN TO USE: 2–6 mutually-exclusive options where per-option context helps.
    For yes/no, use `confirm`. For mixed inputs, use `form`.

    WRITE OPTIONS:
    - Label: noun or short imperative, ≤ 5 words, no punctuation, no emoji.
    - Description: one sentence stating the trade-off or consequence.
    - Keep options parallel in grammar.
    - For visual choice ("which of these images?") add `thumbnail` per
      option — same `src` rules as everywhere else (data: URL, http(s)
      URL, or absolute / `~/` local path on YOUR host). aiui resolves
      paths and URLs to data: URLs before render.

    ANTI-PATTERNS: > 8 options (use `form` with a `list` field); generic labels
    like "Option 1"; redundant descriptions that just restate the label.

    Returns `{cancelled, answers, other?}`. `answers` is a list of values.

    Args:
        question: Full question, imperative or interrogative.
        options: List of `{"label": str, "description"?: str, "value"?: str,
            "thumbnail"?: str}`.
        header: Short chip above the question (≤ 14 chars).
        multi_select: Allow selecting multiple options.
        allow_other: Offer a free-text fallback.
    """
    spec = {
        "kind": "ask",
        "question": question,
        "header": header,
        "options": options,
        "multiSelect": multi_select,
        "allowOther": allow_other,
    }
    return _format_result(await _post_render(spec))


@mcp.tool()
async def form(
    title: str,
    fields: list[dict[str, Any]] | None = None,
    description: str | None = None,
    header: str | None = None,
    tabs: list[dict[str, Any]] | None = None,
    actions: list[dict[str, Any]] | None = None,
    submit_label: str | None = None,
    cancel_label: str | None = None,
) -> dict[str, Any]:
    """Whenever the user needs to provide ≥ 2 related inputs, or any single
    input that doesn't belong in chat (secret, date/datetime/range,
    bounded number, sortable ranking, multi-select, color pick,
    table-row triage with column context, image confirm/grid), call
    this tool instead of typing the questions one by one.

    WHEN TO USE: ≥ 2 related inputs, or one input plus context/confirmation.
    For yes/no, use `confirm`. For a single choice, use `ask`.

    WRITE LABELS:
    - Imperative or noun, ≤ 6 words, no punctuation, no emoji.
    - Consistent register across all fields.
    - Field-level descriptions only if the label alone is ambiguous.

    BE RESTRAINT:
    - ≤ 8 fields per dialog. Split logically if you need more.
    - `static_text` only for context the user couldn't derive from labels.
    - Defaults that a human would actually pick.

    ACTION BUTTONS:
    - Verb-based and concrete ("Create report"), not "OK".
    - Styling variants (pick one per button):
      - `primary: true`  → blue, default emphasis for the main action.
      - `success: true`  → green, for positive-outcome actions ("Approve", "Publish", "Accept").
      - `destructive: true` → red, for deletions/force-pushes/rollbacks.
      - none → neutral outlined button.
      Never red a save button. Never green a delete button.
    - `skip_validation: true` on escape hatches so required-field validation
      doesn't trap the user.
    - ≤ 3 actions.

    FIELD KINDS:
    - text:        {kind, name, label, placeholder?, default?, multiline?, required?}
    - password:    {kind, name, label, placeholder?, required?}  — masked on screen only; value returns as plaintext in the response. Use for short-lived secrets; direct users to keychain/env for long-lived ones.
    - number:      {kind, name, label, default?, min?, max?, step?, required?}
    - select:      {kind, name, label, options: [{label, value}], default?, required?}
    - checkbox:    {kind, name, label, default?}
    - slider:      {kind, name, label, min, max, step?, default?}
    - date:        {kind, name, label, default?, required?}  — ISO YYYY-MM-DD
    - datetime:    {kind, name, label, default?, required?}  — ISO YYYY-MM-DDTHH:MM
    - date_range:  {kind, name, label, default?: {from, to}, required?}  — result {from, to}
    - color:       {kind, name, label, default?}  — hex "#RRGGBB"
    - static_text: {kind, text, tone?: "info"|"warn"|"muted"}  — display only
    - markdown:    {kind, text}  — read-only Markdown block; only as inline context for following inputs in the same form, NOT as a standalone display tool.
    - image:       {kind, src, label?, alt?, max_height?}  — read-only image. `src` accepts an absolute / `~/` local path (read on YOUR host), an `http(s)://` URL (fetched on the Mac), or a `data:` URL. Use for visual confirmation of agent-generated previews.
    - mermaid:     {kind, source, label?, max_height?}  — read-only Mermaid diagram (flowchart, sequence, state, gantt, mindmap, …). `source` is a Mermaid-DSL string. Use this instead of ASCII / box-drawing art when you'd otherwise sketch a diagram in chat — aiui renders to SVG and DOMPurify-sanitises before display.
    - wireframe:   {kind, panels: [{title?, content?, col_span?, row_span?, tone?}], columns?, gap?, label?, max_height?}  — read-only UI-layout mockup. Real CSS-Grid panels with optional header (`title`) and multi-line monospace body (`content`, escape `\n`). `tone` ∈ {"default","muted","highlight"}. Use this for *UI-layouts* (dashboard tiles, hardware-UI panels, login screens, anything with fixed-position boxes-and-labels) instead of ASCII boxes-and-pipes — `mermaid` is for *diagrams* (graphs, sequence/state, gantt). Wireframe complements it for the layout class.
    - image_grid:  {kind, name, label?, images: [{value, src, label?}], multi_select?, columns?, default_selected?, required?}
      Result: {selected: [values]}
    - list:        {kind, name, label?, items: [{label, value, description?, thumbnail?}],
                    selectable?, multi_select?, sortable?, default_selected?: [values]}
      Result: {selected: [values], order: [values]}. Thumbnails optional per item.
    - table:       {kind, name, label?, columns: [{key, label, align?}], rows: [{value, values}],
                    multi_select?, sortable_by_column?, default_selected?, required?}
      Result: {selected: [values], order: [values], sort: {column, dir}}
    - tree:        {kind, name, label?, items: [{label, value, description?, children?: [...]}],
                    multi_select?, default_selected?: [values], default_expanded?: [values]}
      Result: {selected: [values]}

    TABS (optional grouping for long forms):
    Pass `tabs=[{"label": ..., "fields": [...]}, ...]` instead of (or alongside,
    but `tabs` wins) `fields`. One submit covers all tabs; validation jumps to
    the first invalid tab. Tabs structure presentation only — they are not a
    wizard, no per-tab confirmation.

    Returns `{cancelled, action?, values: {name: value, ...}}`.

    Args:
        title: Window title. Same rules as labels.
        fields: List of field blocks, each with a `kind` from above. Use this
            OR `tabs`, not both.
        description: Subtitle, ≤ 2 sentences.
        header: Chip above the title (≤ 14 chars).
        tabs: Tab-grouped field list `[{label, fields}]` for longer forms.
        actions: Footer buttons `[{label, value, primary?, success?, destructive?, skip_validation?}]`.
            Styling variants are mutually exclusive; pick one of primary/success/destructive or leave all off for neutral.
            Without actions, defaults to Cancel + Submit.
        submit_label: Legacy fallback for the default submit button label.
        cancel_label: Legacy fallback for the default cancel button label.
    """
    spec = {
        "kind": "form",
        "title": title,
        "description": description,
        "header": header,
        "fields": fields,
        "tabs": tabs,
        "actions": actions,
        "submitLabel": submit_label,
        "cancelLabel": cancel_label,
    }
    return _format_result(await _post_render(spec))


@mcp.tool()
async def confirm(
    title: str,
    message: str | None = None,
    header: str | None = None,
    destructive: bool = False,
    confirm_label: str | None = None,
    cancel_label: str | None = None,
    image: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Before writing any yes/no question into chat, call this tool instead.
    Pass `destructive=True` (red button) for delete / drop / force-push /
    rollback / prod-deploy — never trust loose prior approval for
    irreversible steps; re-confirm in a dialog.

    WHEN TO USE: irreversible or high-stakes step where "just proceed" is
    unsafe. For pure information, respond in chat. For 3+ options, use `ask`.
    For visual sign-off ("is this generated image OK?"), pass `image`.

    WRITE:
    - Title: the decision as a question, ≤ 10 words.
    - Message: one sentence stating the concrete consequence.
    - `destructive=True` for deletions/force-pushes/rollbacks — never for
      saves or creates.
    - Custom `confirm_label`/`cancel_label` when verbs clarify.
    - `image` for visual confirmation — same `src` rules as elsewhere
      (data: URL, http(s) URL, or absolute / `~/` local path on YOUR host).

    Returns `{cancelled, confirmed}`. `cancelled=True` means Escape or window
    close. `cancelled=False, confirmed=False` means the explicit No button.

    Args:
        title: The decision phrased as a question.
        message: One-sentence explanation of what happens on confirm.
        header: Chip above the title.
        destructive: Red confirm button.
        confirm_label: Defaults to "Ja".
        cancel_label: Defaults to "Nein".
        image: `{"src": str, "alt"?: str, "max_height"?: int}`. Shown above
            the title for visual confirmation. `src` follows the standard
            aiui resolution rules.
    """
    spec = {
        "kind": "confirm",
        "title": title,
        "message": message,
        "header": header,
        "destructive": destructive,
        "confirmLabel": confirm_label,
        "cancelLabel": cancel_label,
        "image": image,
    }
    return _format_result(await _post_render(spec))


@mcp.prompt(name="teach")
def teach_prompt() -> str:
    """Brief the agent on aiui. Loads the full widget catalog, design
    rules, and anti-patterns into the session. Run once per project so
    the agent reaches for the right dialog without further prompting."""
    try:
        return (resources.files("aiui_mcp") / "skill.md").read_text()
    except Exception:
        return (
            "aiui skill doc not bundled with this install. "
            "See https://github.com/byte5ai/aiui/blob/main/docs/skill.md"
        )


# Prompt texts kept in sync verbatim with the Rust MCP server
# (companion/src-tauri/src/mcp.rs) so /aiui:update and /aiui:version behave
# identically whether the user is on the native app MCP or on PyPI via uvx.

_UPDATE_PROMPT = """\
Check whether an aiui update is available and install it if so. Call the \
`update` tool now, then report back concisely:

- If `updated: true`, report "aiui updated {current} -> {available}" and \
  mention that aiui will relaunch itself silently; the next agent call \
  will hit the new version.
- If `updated: false` and `note: "already on latest"`, report "aiui is \
  on the latest version ({current})".
- If `error` is set, report the error verbatim.

Keep the reply to one short sentence unless the user asked for detail.
"""

_VERSION_PROMPT = """\
Report the current aiui version to the user. Call the `version` tool and \
reply with one short line containing the version plus the build date \
parsed from `build_info` (format "v{ver} (commit, yyyy-mm-dd)"). If the \
user asked for more, include the binary path and updater endpoint.
"""


@mcp.prompt()
def update() -> str:
    """Instructs the agent to call `update` and report the outcome.

    Wired up so Claude Code exposes `/aiui:update` as a slash-command that
    triggers a silent update check + install on the user's Mac. Works both
    locally (MCP talks to aiui on localhost) and remotely (MCP calls reach
    aiui through the SSH reverse-tunnel — the update runs on the user's Mac,
    not on the remote host)."""
    return _UPDATE_PROMPT


@mcp.prompt()
def version() -> str:  # noqa: A001  — shadowing by design; prompt names surface as `/aiui:version`
    """Instructs the agent to call `version` and report the current aiui
    companion version in a single line."""
    return _VERSION_PROMPT


_HEALTH_PROMPT = """\
Run the `aiui_health` tool and report the result in one short sentence:

- If `ready: true`, say "aiui ready (v{version})".
- If `ready: false`, point at the most likely cause based on the response \
  body (WebView frozen, dialog backlog, too many children) and suggest the \
  one-step fix ("open Settings, click Check for updates" or "restart aiui").

Don't dump the raw JSON unless the user asked for it.
"""

_TEST_DIALOG_PROMPT = """\
Open a small demo dialog so the user can verify aiui is wired up end to \
end. Call the `confirm` tool with:

  title: "aiui test dialog"
  message: "Click any button — this just verifies the wiring."
  header: "Demo"
  confirm_label: "It works"
  cancel_label: "Close"

Report the outcome in one line: "aiui ok — you clicked '{label}'" if the \
window opened and returned, or the underlying error if it didn't.
"""

_REMOTES_PROMPT = """\
Show the user a quick rundown of their registered aiui remotes — same set \
the Settings window's "Eingerichtete Remote-Hosts" section shows, but in \
chat. Call `aiui_health` first to confirm aiui is up; if it isn't, just \
tell the user that and stop. Otherwise read \
`~/.config/aiui/remotes.json` (JSON array of host strings) and present \
the entries in a compact list. If the file is missing or empty, say "no \
remotes registered yet — open Settings to add one".
"""


@mcp.prompt(name="health")
def health_prompt() -> str:
    """Instructs the agent to call `aiui_health` and report the result in
    one short sentence. Surfaces as `/aiui:health` in Claude Code."""
    return _HEALTH_PROMPT


@mcp.prompt(name="test-dialog")
def test_dialog_prompt() -> str:
    """Demo dialog so the user can verify aiui is wired up end to end.
    Surfaces as `/aiui:test-dialog` in Claude Code."""
    return _TEST_DIALOG_PROMPT


@mcp.prompt(name="remotes")
def remotes_prompt() -> str:
    """Quick rundown of registered aiui remotes in chat (same set the
    Settings window shows). Surfaces as `/aiui:remotes` in Claude Code."""
    return _REMOTES_PROMPT


@mcp.tool()
async def aiui_health() -> dict[str, Any]:
    """Reachability + token check against the aiui companion.

    Use this first if dialogs hang or fail — it distinguishes a cold companion
    (user needs to launch Claude Desktop, or the SSH tunnel is down) from a
    rogue local process holding the port with the wrong token.
    """
    try:
        async with httpx.AsyncClient(timeout=HEALTH_TIMEOUT_S) as client:
            r = await client.get(
                f"{ENDPOINT}/health",
                headers={"Authorization": f"Bearer {_token()}"},
            )
            r.raise_for_status()
            data = r.json()
            return {"ok": True, **data, "endpoint": ENDPOINT, "server": BUILD_INFO}
    except Exception as e:
        log.warning("health check failed: %s", e)
        return {
            "ok": False,
            "error": _explain_exc(e),
            "endpoint": ENDPOINT,
            "server": BUILD_INFO,
        }


# Renamed from the FastMCP-decorator `version` prompt — tools use a
# differently-scoped namespace, so no collision, but this aliasing makes the
# intent explicit in logs.
@mcp.tool(name="version")
async def version_tool() -> dict[str, Any]:
    """Report aiui companion version, build info, binary path, and updater endpoint.

    Cheap; does not hit the network. Works against both a local companion
    (on-Mac) and a remote one reached via SSH tunnel.
    """
    try:
        async with httpx.AsyncClient(timeout=HEALTH_TIMEOUT_S) as client:
            r = await client.get(
                f"{ENDPOINT}/version",
                headers={"Authorization": f"Bearer {_token()}"},
            )
            r.raise_for_status()
            return r.json()
    except Exception as e:
        # Same defensive wrapping as aiui_health: a bare exception with an
        # empty message would surface as "Error executing tool version:" in
        # the client and leave the user with nothing to act on. Mirror the
        # diagnosis aiui_health gives.
        raise RuntimeError(
            f"aiui /version failed at {ENDPOINT}: {_explain_exc(e)}. "
            f"Run `aiui_health` for a full reachability diagnosis."
        ) from e


@mcp.tool(name="update")
async def update_tool() -> dict[str, Any]:
    """Check for an aiui update on the user's Mac and install it silently.

    Responds BEFORE the companion schedules its relaunch, so the caller
    receives `{updated, current, available, note}`. Next agent call hits
    the new version.

    Runs the updater against the *user's Mac*, regardless of whether the
    MCP is local or reached via an SSH reverse-tunnel — because the
    /update HTTP endpoint lives on the aiui.app companion, not on this
    process.
    """
    # Use the long render timeout because download + install of the updater
    # bundle can take several seconds on a slow network.
    try:
        async with httpx.AsyncClient(timeout=TIMEOUT_S) as client:
            r = await client.post(
                f"{ENDPOINT}/update",
                headers={"Authorization": f"Bearer {_token()}"},
            )
            r.raise_for_status()
            return r.json()
    except Exception as e:
        raise RuntimeError(
            f"aiui /update failed at {ENDPOINT}: {_explain_exc(e)}. "
            f"Run `aiui_health` first to check whether aiui.app is reachable."
        ) from e


def main() -> None:
    """Entry point for the `aiui-mcp` console script. Default transport is
    stdio (what Claude Code expects). Legacy `--stdio` flag is accepted for
    compatibility with the old script-based invocation."""
    # stdio is the only transport we support; flag-parsing kept minimal.
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
