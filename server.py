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

VERSION = "0.1.0"


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
    """Zeigt eine Frage mit Optionen als nativen Dialog am User-Mac.

    Like AskUserQuestion, but routed through the aiui companion so you can
    layer more UI on top later. Returns `{cancelled, answers, other?}`.

    Args:
        question: Die vollständige Frage.
        options: Liste von {"label": "...", "description": "...", "value": "..."}.
            `description` und `value` sind optional; bei fehlendem value wird `label` zurückgegeben.
        header: Kurzer Chip/Tag über der Frage (max ~14 Zeichen).
        multi_select: Mehrfachauswahl erlauben.
        allow_other: "Andere Antwort"-Freitextfeld anbieten.
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
    """Zeigt ein freies Fenster mit kombinierbaren UI-Primitives.

    Kompositions-Philosophie: Der Agent baut ein vollständiges Fenster aus
    Feld-Blöcken und Action-Buttons. Returns `{cancelled, action?, values: {name: value}}`.

    Args:
        title: Titel des Fensters.
        fields: Liste von Feld-Blöcken. Unterstützte `kind`-Werte:
            - text:        {kind, name, label, placeholder?, default?, multiline?, required?}
            - password:    {kind, name, label, placeholder?, required?}  (Eingabe maskiert)
            - number:      {kind, name, label, default?, min?, max?, step?, required?}
            - select:      {kind, name, label, options: [{label, value}], default?, required?}
            - checkbox:    {kind, name, label, default?}
            - slider:      {kind, name, label, min, max, step?, default?}
            - date:        {kind, name, label, default?, required?}
            - static_text: {kind, text, tone?: "info"|"warn"|"muted"}  (reine Anzeige)
            - list:        {kind, name, label?, items: [{label, value, description?}],
                            selectable?, multi_select?, sortable?, default_selected?: [values]}
              Result: {selected: [values], order: [values]}
        description: Untertitel (optional).
        header: Chip/Tag oberhalb des Titels (optional).
        actions: Liste von Buttons im Footer (überschreibt submit_label/cancel_label):
            [{label, value, primary?, destructive?, skip_validation?}, ...]
            Result enthält `action: <value>`. Ohne actions: Default ist Abbrechen + Senden.
        submit_label: Legacy — Beschriftung des Default-Submit-Buttons.
        cancel_label: Legacy — Beschriftung des Default-Abbrechen-Buttons.
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
    """Zeigt einen Ja/Nein-Dialog.

    Returns `{cancelled, confirmed}`. `cancelled=True` wenn User Escape drückt
    oder das Fenster schließt; bei explizitem Klick auf Nein: `cancelled=False, confirmed=False`.

    Args:
        title: Überschrift des Dialogs.
        message: Erklärtext (optional).
        header: Chip/Tag (optional).
        destructive: Rot-gefärbten Confirm-Button anzeigen (für löschende Aktionen).
        confirm_label: Default "Ja".
        cancel_label: Default "Nein".
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
