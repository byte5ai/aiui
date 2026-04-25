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

import importlib.metadata
import importlib.resources as resources
import logging
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


TOKEN_PATH = Path(os.environ.get("AIUI_TOKEN_PATH", "~/.config/aiui/token")).expanduser()
ENDPOINT = os.environ.get("AIUI_ENDPOINT", "http://127.0.0.1:7777")
TIMEOUT_S = float(os.environ.get("AIUI_TIMEOUT_S", "120"))
HEALTH_TIMEOUT_S = float(os.environ.get("AIUI_HEALTH_TIMEOUT_S", "3"))

mcp = FastMCP("aiui")


def _token() -> str:
    if not TOKEN_PATH.exists():
        raise RuntimeError(
            f"aiui token not found at {TOKEN_PATH}. "
            "Install the aiui companion on your Mac and register this remote from its "
            "settings window (adds the token automatically). "
            "Download: https://github.com/byte5ai/aiui/releases/latest"
        )
    return TOKEN_PATH.read_text().strip()


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
                f"this host. ({e})"
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


async def _post_render(spec: dict[str, Any]) -> dict[str, Any]:
    await _preflight()
    t0 = datetime.now(timezone.utc)
    log.info("render → kind=%s", spec.get("kind"))
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
    options: list[dict[str, str]],
    header: str | None = None,
    multi_select: bool = False,
    allow_other: bool = True,
) -> dict[str, Any]:
    """USE WHEN you would otherwise list options in chat and wait for the user
    to type back which one — picking a deploy strategy, a migration path,
    a file to act on, etc. Renders a native macOS choice window with
    per-option descriptions, optional multi-select and free-text fallback.

    WHEN TO USE: 2–6 mutually-exclusive options where per-option context helps.
    For yes/no, use `confirm`. For mixed inputs, use `form`.

    WRITE OPTIONS:
    - Label: noun or short imperative, ≤ 5 words, no punctuation, no emoji.
    - Description: one sentence stating the trade-off or consequence.
    - Keep options parallel in grammar.

    ANTI-PATTERNS: > 8 options (use `form` with a `list` field); generic labels
    like "Option 1"; redundant descriptions that just restate the label.

    Returns `{cancelled, answers, other?}`. `answers` is a list of values.

    Args:
        question: Full question, imperative or interrogative.
        options: List of `{"label": str, "description"?: str, "value"?: str}`.
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
    fields: list[dict[str, Any]],
    description: str | None = None,
    header: str | None = None,
    actions: list[dict[str, Any]] | None = None,
    submit_label: str | None = None,
    cancel_label: str | None = None,
) -> dict[str, Any]:
    """USE WHEN the user needs to give you ≥ 2 related inputs, or any single
    input that's better entered somewhere other than the chat — secrets
    (password, masked on screen), dates and ranges, bounded numbers (slider),
    sortable rankings, multi-selects, color picks. Renders a native macOS
    form window with multiple typed fields + multiple action buttons.

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
    - date_range:  {kind, name, label, default?: {from, to}, required?}  — result {from, to}
    - color:       {kind, name, label, default?}  — hex "#RRGGBB"
    - static_text: {kind, text, tone?: "info"|"warn"|"muted"}  — display only
    - list:        {kind, name, label?, items: [{label, value, description?}],
                    selectable?, multi_select?, sortable?, default_selected?: [values]}
      Result: {selected: [values], order: [values]}
    - tree:        {kind, name, label?, items: [{label, value, description?, children?: [...]}],
                    multi_select?, default_selected?: [values], default_expanded?: [values]}
      Result: {selected: [values]}

    Returns `{cancelled, action?, values: {name: value, ...}}`.

    Args:
        title: Window title. Same rules as labels.
        fields: List of field blocks, each with a `kind` from above.
        description: Subtitle, ≤ 2 sentences.
        header: Chip above the title (≤ 14 chars).
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
) -> dict[str, Any]:
    """USE WHEN you would otherwise ask the user a yes/no question in chat —
    and ALWAYS before any irreversible step (delete, drop, force-push,
    rollback, prod deploy). Renders a native macOS yes/no window; pass
    `destructive=True` for a red confirm button on dangerous actions.

    WHEN TO USE: irreversible or high-stakes step where "just proceed" is
    unsafe. For pure information, respond in chat. For 3+ options, use `ask`.

    WRITE:
    - Title: the decision as a question, ≤ 10 words.
    - Message: one sentence stating the concrete consequence.
    - `destructive=True` for deletions/force-pushes/rollbacks — never for
      saves or creates.
    - Custom `confirm_label`/`cancel_label` when verbs clarify.

    Returns `{cancelled, confirmed}`. `cancelled=True` means Escape or window
    close. `cancelled=False, confirmed=False` means the explicit No button.

    Args:
        title: The decision phrased as a question.
        message: One-sentence explanation of what happens on confirm.
        header: Chip above the title.
        destructive: Red confirm button.
        confirm_label: Defaults to "Ja".
        cancel_label: Defaults to "Nein".
    """
    spec = {
        "kind": "confirm",
        "title": title,
        "message": message,
        "header": header,
        "destructive": destructive,
        "confirmLabel": confirm_label,
        "cancelLabel": cancel_label,
    }
    return _format_result(await _post_render(spec))


@mcp.prompt()
def widgets() -> str:
    """The full aiui widget catalog — when to use which dialog, copy
    conventions, anti-patterns, example payloads. Read before composing the
    first dialog in a session."""
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
        return {"ok": False, "error": str(e), "endpoint": ENDPOINT, "server": BUILD_INFO}


# Renamed from the FastMCP-decorator `version` prompt — tools use a
# differently-scoped namespace, so no collision, but this aliasing makes the
# intent explicit in logs.
@mcp.tool(name="version")
async def version_tool() -> dict[str, Any]:
    """Report aiui companion version, build info, binary path, and updater endpoint.

    Cheap; does not hit the network. Works against both a local companion
    (on-Mac) and a remote one reached via SSH tunnel.
    """
    async with httpx.AsyncClient(timeout=HEALTH_TIMEOUT_S) as client:
        r = await client.get(
            f"{ENDPOINT}/version",
            headers={"Authorization": f"Bearer {_token()}"},
        )
        r.raise_for_status()
        return r.json()


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
    async with httpx.AsyncClient(timeout=TIMEOUT_S) as client:
        r = await client.post(
            f"{ENDPOINT}/update",
            headers={"Authorization": f"Bearer {_token()}"},
        )
        r.raise_for_status()
        return r.json()


def main() -> None:
    """Entry point for the `aiui-mcp` console script. Default transport is
    stdio (what Claude Code expects). Legacy `--stdio` flag is accepted for
    compatibility with the old script-based invocation."""
    # stdio is the only transport we support; flag-parsing kept minimal.
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
