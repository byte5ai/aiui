"""Tests for the async-render client path (Step 3): the bridge POSTs, then
polls `GET /render/{id}` until a terminal result, emitting progress on the way.
"""
from __future__ import annotations

import asyncio
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import _cancel_render, _poll_render, _wait_for_aiui


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
    token_file.write_text("de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57")
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


def test_poll_render_retries_transport_error_then_succeeds(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """#193: a tunnel blip on the poll GET must cost one retry, not the answer.

    Safe only because the companion's delivery is idempotent — the terminal
    result is still there on the second GET.
    """
    _setup_token(monkeypatch, tmp_path)
    monkeypatch.setattr(server, "ASYNC_POLL_RETRY_BACKOFF_S", 0.0)
    calls = {"n": 0}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        calls["n"] += 1
        if calls["n"] == 1:
            raise httpx.ReadError("tunnel blip")
        return _FakeResp({"id": "x", "cancelled": False, "result": {"values": {"pw": "s3cr3t"}}})

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    async def run() -> dict[str, Any]:
        async with httpx.AsyncClient() as client:
            return await _poll_render(client, "x", None)

    data = asyncio.run(run())
    assert data["result"]["values"]["pw"] == "s3cr3t"
    assert calls["n"] == 2  # one failed GET, one retry


def test_poll_render_gives_up_after_the_retry_budget(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    monkeypatch.setattr(server, "ASYNC_POLL_RETRY_BACKOFF_S", 0.0)
    calls = {"n": 0}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        calls["n"] += 1
        raise httpx.ReadError("tunnel down")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    async def run() -> dict[str, Any]:
        async with httpx.AsyncClient() as client:
            return await _poll_render(client, "x", None)

    # #202: give-up is now bounded by consecutive transport failures and the
    # error is wrapped in an actionable RuntimeError (not the bare ReadError),
    # so the agent is told the dialog may still be open on the Mac.
    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(run())
    assert "consecutive poll failures" in str(exc_info.value)
    assert calls["n"] == server.ASYNC_POLL_MAX_CONSECUTIVE_FAILURES


def test_poll_render_does_not_retry_404(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A 404 is terminal — the retry is for transport errors only."""
    _setup_token(monkeypatch, tmp_path)
    calls = {"n": 0}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        calls["n"] += 1
        return _FakeResp({"error": "unknown_render_id"}, status=404)

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    async def run() -> dict[str, Any]:
        async with httpx.AsyncClient() as client:
            return await _poll_render(client, "gone", None)

    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(run())
    assert "lost track" in str(exc_info.value)
    assert calls["n"] == 1


def test_poll_render_deletes_render_on_cancellation(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """#193: the SDK cancels the task on a CancelledNotification (Esc in Claude
    Code); without this the task died and the dialog stayed on the Mac."""
    _setup_token(monkeypatch, tmp_path)
    deleted: list[str] = []

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        await asyncio.sleep(30)  # park in the poll, as a live dialog does
        return _FakeResp({"pending": True})

    async def fake_delete(self: Any, url: str, **kwargs: Any) -> Any:
        deleted.append(url)
        return _FakeResp({}, status=204)

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    monkeypatch.setattr(httpx.AsyncClient, "delete", fake_delete)

    async def run() -> None:
        async with httpx.AsyncClient() as client:
            task = asyncio.ensure_future(_poll_render(client, "x", None))
            await asyncio.sleep(0)  # let it reach the GET
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
            # The shielded DELETE outlives the cancelled task — give it a tick.
            for _ in range(5):
                await asyncio.sleep(0)

    asyncio.run(run())
    assert deleted == [f"{server.ENDPOINT}/render/x"]


def test_cancel_render_swallows_old_companion_405(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """An older companion has no DELETE route — nothing to do, never an error."""
    _setup_token(monkeypatch, tmp_path)

    for status in (404, 405):
        async def fake_delete(self: Any, url: str, _s: int = status, **kwargs: Any) -> Any:
            return _FakeResp({}, status=_s)

        monkeypatch.setattr(httpx.AsyncClient, "delete", fake_delete)
        asyncio.run(_cancel_render("x"))  # must not raise

    async def boom(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.ConnectError("companion gone")

    monkeypatch.setattr(httpx.AsyncClient, "delete", boom)
    asyncio.run(_cancel_render("x"))  # a dead companion is not an error either


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
