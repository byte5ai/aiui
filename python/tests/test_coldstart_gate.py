"""Which tools wait out a cold companion, and which one must not (#203).

`_wait_for_aiui` polls the unauthenticated `/ping` for up to
`COLDSTART_WAIT_S` so a companion that is still starting — Claude Desktop
just launched, SSH tunnel just re-established, mcp-stdio mid auto-resurrect —
gets time to begin serving. The render path ran it; `notify`, `version` and
`update` went straight to the HTTP call and failed instantly, while the Rust
bridge gates *every* tool before its dispatch match. `notify` is the worst
place for that gap: it is by definition called when a long task finishes,
which is exactly when the tunnel may have just come back.

`aiui_health` is the deliberate exception — it is the diagnostic and must
answer fast.
"""

from __future__ import annotations

import asyncio
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import aiui_health, notify, update_tool, version_tool


class _FakeResp:
    def __init__(self, payload: dict[str, Any]) -> None:
        self.status_code = 200
        self._payload = payload
        self.text = str(payload)

    def json(self) -> dict[str, Any]:
        return self._payload

    def raise_for_status(self) -> None:
        return None


@pytest.fixture()
def gate(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> list[str]:
    """Record the call order: the gate, then the HTTP verb the tool used."""
    token_file = tmp_path / "token"
    token_file.write_text("de1e7e57" * 8)
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)

    calls: list[str] = []

    async def fake_wait() -> None:
        calls.append("wait")

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        calls.append("get")
        return _FakeResp({"ok": True})

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        calls.append("post")
        return _FakeResp({"ok": True})

    monkeypatch.setattr(server, "_wait_for_aiui", fake_wait)
    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)
    return calls


def test_notify_waits_for_coldstart(gate: list[str]) -> None:
    asyncio.run(notify(title="Tests green", body="all 57 pass"))
    assert gate == ["wait", "post"]


def test_version_waits_for_coldstart(gate: list[str]) -> None:
    asyncio.run(version_tool())
    assert gate == ["wait", "get"]


def test_update_waits_for_coldstart(gate: list[str]) -> None:
    asyncio.run(update_tool())
    assert gate == ["wait", "post"]


def test_health_does_not_wait_for_coldstart(gate: list[str]) -> None:
    """On purpose: `aiui_health` is what a user runs *because* things hang.
    Spending COLDSTART_WAIT_S before reporting "unreachable" would make the
    diagnostic hang too. Do not "fix" this into parity."""
    asyncio.run(aiui_health())
    assert gate == ["get"]


def test_wait_for_aiui_returns_immediately_when_budget_is_zero(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The real gate, not the recorder: a zero budget must not poll at all,
    which is what keeps the mocked-`post` tests from waiting on a live
    companion."""
    polled = False

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        nonlocal polled
        polled = True
        raise httpx.ConnectError("nothing there")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    monkeypatch.setattr(server, "COLDSTART_WAIT_S", 0.0)

    asyncio.run(server._wait_for_aiui())

    assert polled is False
