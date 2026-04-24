#!/usr/bin/env uv run
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "mcp>=1.26.0",
#     "httpx>=0.27",
# ]
# ///
"""aiui — MCP-Server, der Dialoge an den lokalen Companion (am User-Mac)
schickt.

Topologie:

  Claude Code CLI (Remote) ──stdio──► aiui MCP (dieser Prozess)
                                           │ HTTP
                                           ▼
                                 localhost:7777 (SSH-Reverse-Tunnel)
                                           │
                                           ▼
                                      Mac: aiui-Companion (Tauri)

Tools:
  ask(...)     — AskUserQuestion-Superset (Optionen, multiSelect, freies "Andere")
  form(...)    — freies Formular (text/number/select/checkbox/slider/date)
  confirm(...) — Ja/Nein, optional destruktiv
  aiui_health() — Ping, prüft ob Companion erreichbar ist

Token wird aus ~/.config/aiui/token gelesen (einmalig vom Mac kopiert).
"""
from __future__ import annotations
import logging
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import httpx
from mcp.server.fastmcp import FastMCP

VERSION = "0.2.0"


def _git_sha() -> str:
    try:
        r = subprocess.run(
            ["git", "-C", os.path.dirname(os.path.abspath(__file__)), "rev-parse", "--short=12", "HEAD"],
            capture_output=True, text=True, timeout=1,
        )
        if r.returncode == 0:
            return r.stdout.strip() or "nogit"
    except Exception:
        pass
    return "nogit"


BUILD_INFO = f"aiui-server v{VERSION} (sha:{_git_sha()})"

# Python logging: ISO-Timestamp, Level, Name, Message. Writes to stderr so
# Claude Desktop captures it in the MCP server log panel. First line of each
# process dumps BUILD_INFO.
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
            "Run the pairing setup in the companion app and scp the token here."
        )
    return TOKEN_PATH.read_text().strip()


async def _preflight() -> None:
    """Guard against Remote-Zombies: before each dialog call, confirm that the
    instance on :7777 is actually *our* companion (accepts our bearer token).
    A local aiui process running on the same remote host with a different
    token would hijack the SSH reverse-forward and hang dialogs silently.
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
                f"Is Claude Desktop running and the SSH reverse-tunnel up? ({e})"
            ) from e
        except httpx.ReadTimeout as e:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} timed out on /health — suggests a "
                f"stale local aiui instance holding the port. Run `pkill -f '^aiui$'` "
                f"on this host. ({e})"
            ) from e

        if r.status_code == 401:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} rejected our token (401). "
                f"Remote-Zombie likely: another aiui process is listening on this port "
                f"with a different token. Run `pkill -f '^aiui$'` on this host."
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
    """Single- or multi-choice picker with optional free-text fallback.

    WHEN TO USE: 2–6 mutually-exclusive options where context per option helps
    (description field). For pure yes/no, use `confirm`. For mixed inputs
    (text + choice + slider…), use `form`.

    WRITE OPTIONS:
    - Label: noun or short imperative, ≤ 5 words, no punctuation, no emoji.
    - Description: one sentence stating the trade-off or consequence.
    - Value: snake_case stable identifier if distinct from label.
    - Keep options parallel in grammar ("In-place" / "Blue-green" / "Dump", not
      "In-place" / "We do a blue-green" / "Dump it").

    ANTI-PATTERNS: > 8 options (use `form` with a `list` field instead);
    generic labels like "Option 1" or "Choice A"; redundant descriptions that
    just restate the label.

    Returns `{cancelled, answers, other?}`. `answers` is a list of values in
    selection order; `other` contains the free-text answer if any.

    Args:
        question: The full question, imperative or interrogative.
        options: List of {"label": str, "description"?: str, "value"?: str}.
        header: Short chip above the question (≤ 14 chars). Use sparingly for
            disambiguation (e.g. "Migration", "Refactor").
        multi_select: Allow selecting multiple options.
        allow_other: Offer a free-text "other answer" fallback.
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
    """Composite window: multiple typed fields + multiple action buttons.

    WHEN TO USE: ≥ 2 related inputs, or one input plus context/confirmation.
    For a single yes/no, use `confirm`. For a single choice, use `ask`.

    WRITE LABELS:
    - Imperative or noun, ≤ 6 words, no punctuation, no emoji.
    - Consistent register across all fields (don't mix "Name" with "Bitte
      geben Sie Ihr Alter ein").
    - Field-level descriptions only if the label alone is ambiguous.

    BE RESTRAINT:
    - ≤ 8 fields in one dialog. If you need more, split into multiple dialogs
      or use a `static_text` separator and group logically.
    - `static_text` only for context the user couldn't derive from labels;
      don't echo the title or re-explain the action buttons.
    - Defaults that a human would actually pick, not "enter value here".

    ACTION BUTTONS:
    - Verb-based, concrete ("Bericht erstellen" / "Create report"), not "OK".
    - Destructive actions: set `destructive: true` — never red a save button.
    - `skip_validation: true` on escape hatches ("Später", "Entwurf", "Cancel")
      so the user isn't trapped if required fields are empty.
    - ≤ 3 actions.

    FIELD KINDS:
    - text:        {kind, name, label, placeholder?, default?, multiline?, required?}
    - password:    {kind, name, label, placeholder?, required?}  — masked on screen only; value returns as plaintext. Short-lived secrets OK; long-lived → send user to keychain/env.
    - number:      {kind, name, label, default?, min?, max?, step?, required?}
    - select:      {kind, name, label, options: [{label, value}], default?, required?}
    - checkbox:    {kind, name, label, default?}
    - slider:      {kind, name, label, min, max, step?, default?}
    - date:        {kind, name, label, default?, required?}  — ISO YYYY-MM-DD
    - date_range:  {kind, name, label, default?: {from, to}, required?}  — result {from, to}
    - color:       {kind, name, label, default?}  — hex "#RRGGBB"
    - static_text: {kind, text, tone?: "info"|"warn"|"muted"}  — no input, display-only
    - list:        {kind, name, label?, items: [{label, value, description?}],
                    selectable?, multi_select?, sortable?, default_selected?: [values]}
      Result: {selected: [values], order: [values]}
    - tree:        {kind, name, label?, items: [{label, value, description?, children?: [...]}],
                    multi_select?, default_selected?: [values], default_expanded?: [values]}
      Result: {selected: [values]}. Hierarchical — for paths, categories, nested pickers.

    Returns `{cancelled, action?, values: {name: value, ...}}`.

    Args:
        title: Window title. Same rules as labels.
        fields: List of field blocks, each with a `kind` from above.
        description: Subtitle, ≤ 2 sentences. Keep short — labels carry the weight.
        header: Chip above the title (≤ 14 chars).
        actions: Footer buttons [{label, value, primary?, destructive?, skip_validation?}].
            Without actions, defaults to Cancel + Submit.
        submit_label: Legacy — label of the default submit button.
        cancel_label: Legacy — label of the default cancel button.
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
    """Hard yes/no decision with optional destructive styling.

    WHEN TO USE: irreversible or high-stakes step where "just proceed" is
    unsafe. For plain information display, do not use a dialog — respond in
    chat. For a choice between 3+ options, use `ask`.

    WRITE:
    - Title: the decision as a question, ≤ 10 words ("Migration starten?").
    - Message: one sentence stating the concrete consequence ("15 Tabellen
      werden umgezogen, Dauer ca. 3 Min.").
    - `destructive=True` for deletions, force-pushes, rollbacks — never for
      saves or creates.
    - `confirm_label` / `cancel_label` if verbs clarify ("Löschen" / "Behalten"
      is better than "Ja" / "Nein" for destructive actions).

    Returns `{cancelled, confirmed}`. `cancelled=True` means the user pressed
    Escape or closed the window. `cancelled=False, confirmed=False` means
    they clicked the explicit No.

    Args:
        title: The decision phrased as a question.
        message: One-sentence explanation of what happens on confirm.
        header: Chip above the title (optional).
        destructive: Red confirm button for destructive actions.
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
    """Returns the full aiui widget catalog — when to use which dialog, copy
    conventions, anti-patterns, example payloads. Use this before composing
    the first dialog in a session."""
    skill_path = Path(__file__).parent / "docs" / "skill.md"
    if skill_path.exists():
        return skill_path.read_text()
    return "aiui skill doc not bundled with this install. See https://github.com/byte5ai/aiui/blob/main/docs/skill.md"


@mcp.tool()
async def aiui_health() -> dict[str, Any]:
    """Prüft, ob der Companion am User-Mac erreichbar ist (via SSH-Tunnel)."""
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


if __name__ == "__main__":
    if "--stdio" in sys.argv:
        mcp.run(transport="stdio")
    else:
        sys.exit("Usage: uv run server.py --stdio")
