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


def test_poll_render_raises_on_404(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
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
    _fake_clock(monkeypatch)
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


class _FakeClock:
    """Drives `_poll_render` on fake time (E-01): `_now` reads it, `_sleep`
    advances it. A GET can advance it too, to model a request that hangs
    until its timeout."""

    def __init__(self) -> None:
        self.t = 1000.0
        self.sleeps: list[float] = []

    def now(self) -> float:
        return self.t

    async def sleep(self, delay: float) -> None:
        self.sleeps.append(delay)
        self.t += delay
        await asyncio.sleep(0)


def _fake_clock(monkeypatch: pytest.MonkeyPatch) -> _FakeClock:
    clock = _FakeClock()
    # `raising=False`: the names are the seam this fix introduced, so the test
    # still reaches its assertion (and fails there) against the old loop.
    monkeypatch.setattr(server, "_now", clock.now, raising=False)
    monkeypatch.setattr(server, "_sleep", clock.sleep, raising=False)
    return clock


def _run_poll(ttl_secs: float = server.DEFAULT_POLL_TTL_S, render_id: str = "x") -> Any:
    async def run() -> dict[str, Any]:
        async with httpx.AsyncClient() as client:
            # A hard real-time cap: the pre-fix loop must fail, never hang.
            return await asyncio.wait_for(
                _poll_render(client, render_id, None, ttl_secs), timeout=10
            )

    return asyncio.run(run())


def test_poll_render_survives_a_refused_port_for_most_of_the_budget(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """E-01: a refused connection fails in milliseconds — the SSH reverse-tunnel
    restarting looks exactly like this. A count budget gave up after ~4 s; the
    wall-clock budget must carry the dialog through ~3 minutes of it."""
    _setup_token(monkeypatch, tmp_path)
    clock = _fake_clock(monkeypatch)
    start = clock.t
    calls = {"n": 0}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        calls["n"] += 1
        if clock.t - start < 170:
            raise httpx.ConnectError("[Errno 111] Connection refused")
        return _FakeResp({"id": "x", "cancelled": False, "result": {"confirmed": True}})

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    data = _run_poll()
    assert data["result"]["confirmed"] is True
    assert clock.t - start >= 170, "the outage really lasted ~170 s of poll time"


def test_poll_render_backoff_is_capped_exponential(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """1, 2, 4, 8, 8, … s between retries — the contract both bridges share."""
    _setup_token(monkeypatch, tmp_path)
    clock = _fake_clock(monkeypatch)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.ConnectError("refused")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    with pytest.raises(RuntimeError):
        _run_poll()
    assert clock.sleeps[:6] == [1.0, 2.0, 4.0, 8.0, 8.0, 8.0]
    assert max(clock.sleeps) == server.ASYNC_POLL_MAX_BACKOFF_S


def test_poll_render_gives_up_once_the_outage_budget_is_spent(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """The retry is bounded by wall-clock time, not by a count — and not
    before the budget either."""
    _setup_token(monkeypatch, tmp_path)
    clock = _fake_clock(monkeypatch)
    start = clock.t

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.ReadError("tunnel down")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    with pytest.raises(RuntimeError) as exc_info:
        _run_poll(render_id="r-42")
    msg = str(exc_info.value)
    # #202: wrapped in an actionable RuntimeError (not the bare ReadError),
    # naming the render and telling the agent the dialog may still be open.
    assert "consecutive poll failures" in msg
    assert "r-42" in msg
    assert "may still be open" in msg
    elapsed = clock.t - start
    assert server.POLL_OUTAGE_BUDGET_S <= elapsed <= server.POLL_OUTAGE_BUDGET_S + 1e-6


def test_poll_render_outage_budget_restarts_after_a_good_poll(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """Two separate 150 s outages with one good poll between them are two
    blips, not one 300 s outage."""
    _setup_token(monkeypatch, tmp_path)
    clock = _fake_clock(monkeypatch)
    start = clock.t
    seen_pending = {"done": False}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        t = clock.t - start
        if t < 150:
            raise httpx.ConnectError("refused")
        if not seen_pending["done"]:
            seen_pending["done"] = True
            return _FakeResp({"pending": True})
        if t < 310:
            raise httpx.ConnectError("refused")
        return _FakeResp({"id": "x", "cancelled": False, "result": {"confirmed": True}})

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    assert _run_poll()["result"]["confirmed"] is True


def test_poll_render_404_after_an_outage_names_the_reaper(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """After an outage, a 404 almost always means the companion closed the
    dialog because nobody polled it — "expired or never registered" sent the
    agent looking for the wrong cause."""
    _setup_token(monkeypatch, tmp_path)
    _fake_clock(monkeypatch)
    calls = {"n": 0}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        calls["n"] += 1
        if calls["n"] <= 3:
            raise httpx.ConnectError("refused")
        return _FakeResp({"error": "unknown_render_id"}, status=404)

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    with pytest.raises(RuntimeError) as exc_info:
        _run_poll()
    msg = str(exc_info.value)
    assert "lost track" in msg
    assert "nobody polled it for too long" in msg


def test_poll_render_ends_as_ttl_expired_even_while_pending(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """E-05: the TTL used to be checked only after a failed poll, so a
    companion answering `{pending: true}` forever held the call open forever.
    Past TTL + grace the bridge retracts the dialog and returns the documented
    `ttl_expired` cancel."""
    _setup_token(monkeypatch, tmp_path)
    clock = _fake_clock(monkeypatch)
    deleted: list[str] = []

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        clock.t += 25  # the companion holds each GET for its poll window
        return _FakeResp({"pending": True})

    async def fake_delete(self: Any, url: str, **kwargs: Any) -> Any:
        deleted.append(url)
        return _FakeResp({}, status=204)

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    monkeypatch.setattr(httpx.AsyncClient, "delete", fake_delete)

    data = _run_poll(ttl_secs=300.0)
    assert data["cancelled"] is True
    assert data["reason"] == "ttl_expired"
    assert server._format_result(data, "form") == {
        "cancelled": True,
        "values": {},
        "reason": "ttl_expired",
    }
    assert deleted == [f"{server.ENDPOINT}/render/x"], "the stale window is retracted"
    assert "x" not in server._LIVE_RENDERS


def _http_resp(status: int, content: bytes) -> httpx.Response:
    return httpx.Response(
        status, content=content, request=httpx.Request("GET", "http://127.0.0.1:7777/render/x")
    )


@pytest.mark.parametrize(
    ("status", "content", "needle"),
    [
        (401, b'{"error":"unauthorized"}', "Re-register this host"),
        (500, b"boom", "HTTP 500"),
        (200, b"<html>not json</html>", "unreadable answer"),
        (200, b"[1, 2]", "non-object answer"),
    ],
)
def test_poll_render_turns_bad_answers_into_named_errors(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any, status: int, content: bytes, needle: str
) -> None:
    """E-03: a 401/5xx or a non-JSON / non-object body on the poll used to
    escape as a raw HTTPStatusError / JSONDecodeError / AttributeError — no
    render id, no re-register guidance, no word that the dialog is still open."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        return _http_resp(status, content)

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    with pytest.raises(RuntimeError) as exc_info:
        _run_poll(render_id="r-7")
    msg = str(exc_info.value)
    assert needle in msg
    assert "r-7" in msg


def test_poll_render_does_not_retry_404(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
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


def test_wait_for_aiui_returns_when_ping_ok(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
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
