"""Tests for the async-render client path (Step 3): the bridge POSTs, then
polls `GET /render/{id}` until a terminal result, emitting progress on the way.
"""
from __future__ import annotations

import asyncio
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import _poll_render, _wait_for_aiui


class _FakeResp:
    def __init__(self, payload: dict[str, Any], status: int = 200) -> None:
        self._payload = payload
        self.status_code = status

    def raise_for_status(self) -> None:
        return None  # no test drives the >=400 path through here

    def json(self) -> dict[str, Any]:
        return self._payload


def _setup_token(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
    token_file = tmp_path / "token"
    token_file.write_text("dummy-token-for-tests")
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)


def test_poll_render_returns_terminal_after_pending(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    seq = [
        _FakeResp({"pending": True}),
        _FakeResp({"pending": True}),
        _FakeResp({"id": "x", "cancelled": False, "result": {"confirmed": True}}),
    ]
    calls = {"n": 0}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        i = min(calls["n"], len(seq) - 1)
        calls["n"] += 1
        return seq[i]

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    async def run() -> dict[str, Any]:
        async with httpx.AsyncClient() as client:
            return await _poll_render(client, "x", None)

    data = asyncio.run(run())
    assert data["cancelled"] is False
    assert data["result"]["confirmed"] is True
    assert calls["n"] == 3  # two pending polls, then the terminal one


def test_poll_render_reports_progress_each_pending_iteration(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    seq = [_FakeResp({"pending": True}), _FakeResp({"id": "x", "cancelled": True})]
    calls = {"n": 0}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        i = min(calls["n"], len(seq) - 1)
        calls["n"] += 1
        return seq[i]

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    class _Ctx:
        def __init__(self) -> None:
            self.ticks: list[float] = []

        async def report_progress(
            self, progress: float, total: Any = None, message: Any = None
        ) -> None:
            self.ticks.append(progress)

    ctx = _Ctx()

    async def run() -> dict[str, Any]:
        async with httpx.AsyncClient() as client:
            return await _poll_render(client, "x", ctx)  # type: ignore[arg-type]

    data = asyncio.run(run())
    assert data["cancelled"] is True
    assert ctx.ticks == [1.0]  # one pending iteration → one progress tick


def test_poll_render_raises_on_404(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        return _FakeResp({"error": "unknown_render_id"}, status=404)

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    async def run() -> dict[str, Any]:
        async with httpx.AsyncClient() as client:
            return await _poll_render(client, "gone", None)

    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(run())
    assert "lost track" in str(exc_info.value)


def test_wait_for_aiui_returns_when_ping_ok(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        return _FakeResp({}, status=200)

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    asyncio.run(_wait_for_aiui())  # returns promptly, no raise


def test_wait_for_aiui_tolerates_unreachable_within_budget(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    monkeypatch.setattr(server, "COLDSTART_WAIT_S", 0.2)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.ConnectError("companion down")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    # Must NOT raise — it falls through after the budget so _preflight can
    # produce the precise diagnosis.
    asyncio.run(_wait_for_aiui())
