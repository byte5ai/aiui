"""Regression tests for the empty-error-string bug shipped before 0.4.31.

Symptom in the field: `aiui_health` returned `{"ok": false, "error": ""}` when
the Mac side reset the HTTP connection mid-response — typically a stale SSH
reverse-tunnel still bound to :7777 with nothing alive behind it, or the
on-Mac mcp-stdio caught between auto-resurrect cycles. The same bug surfaced
on the `version` and `update` tools as a bare `Error executing tool …:` with
no diagnostic detail.

Root cause: httpx raises `RemoteProtocolError("")` for "connection reset by
peer" — `str(e)` is empty, and the server passed that straight through. The
fix is `_explain_exc`, which falls back to the exception class name when
`str(e)` has nothing useful, plus extra `except` branches in `_preflight`
that translate the protocol-level errors into actionable messages.

Since #179 this module also covers the *status-code* half of the same
contract: a degraded-but-serving companion (200 + `ready: false`) must not
block an unrelated session's render, and a companion that really is down must
surface its own `hint` instead of a status code and half a JSON blob.
"""
from __future__ import annotations

import asyncio
import json
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import _explain_exc, _preflight, aiui_health


# ----- _explain_exc unit tests -----

def test_explain_exc_returns_class_name_when_str_empty() -> None:
    """The bug-trigger: httpx exceptions with empty str() must still surface."""
    assert _explain_exc(httpx.RemoteProtocolError("")) == "RemoteProtocolError"


def test_explain_exc_returns_message_when_present() -> None:
    """When httpx provides a message, we use it verbatim."""
    e = httpx.ConnectError("nodename nor servname provided")
    assert "nodename" in _explain_exc(e)


def test_explain_exc_strips_whitespace_and_falls_back() -> None:
    """Whitespace-only messages count as empty for our purposes."""
    assert _explain_exc(httpx.RemoteProtocolError("   ")) == "RemoteProtocolError"


def test_explain_exc_works_for_non_httpx_exceptions() -> None:
    """The helper is a generic safety net, not httpx-specific."""
    assert _explain_exc(RuntimeError("")) == "RuntimeError"
    assert _explain_exc(ValueError("bad")) == "bad"


# ----- aiui_health integration: never returns empty error -----

def _setup_token(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
    """Make the token-file lookup succeed with a dummy bearer."""
    token_file = tmp_path / "token"
    token_file.write_text("de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57")
    monkeypatch.setattr("aiui_mcp.server.TOKEN_PATH", token_file)


def test_aiui_health_surfaces_class_name_for_empty_message_exception(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """End-to-end: when httpx raises RemoteProtocolError(''), the tool result
    must still carry a non-empty error string the user can act on."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.RemoteProtocolError("")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    result = asyncio.run(aiui_health())

    assert result["ok"] is False
    assert result["error"], "error must be non-empty (was the bug before 0.4.31)"
    assert "RemoteProtocolError" in result["error"]


def test_aiui_health_passes_through_message_when_present(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """When httpx does provide a message, we keep it (unchanged behaviour)."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.ConnectError("nodename nor servname provided")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    result = asyncio.run(aiui_health())

    assert result["ok"] is False
    assert "nodename" in result["error"]


# ----- _preflight integration: extended except-chain catches the right classes -----

def test_preflight_translates_remote_protocol_error_to_actionable_runtime_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """_preflight is on the render-path; an empty-message RemoteProtocolError
    used to escape as a bare exception. After the fix it becomes a RuntimeError
    with concrete restart-aiui guidance."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.RemoteProtocolError("")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(_preflight())

    msg = str(exc_info.value)
    assert msg, "the runtime error must carry a message"
    assert "reset" in msg.lower() or "RemoteProtocolError" in msg
    # Actionable guidance the user can follow:
    assert "aiui.app" in msg


def test_preflight_catches_generic_http_error_with_class_name_fallback(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """The catch-all `except httpx.HTTPError` keeps stranger transport errors
    from bubbling up as bare exceptions."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        # WriteError is a sibling of ConnectError/ReadTimeout/RemoteProtocolError
        # under httpx.HTTPError; the explicit branches don't list it.
        raise httpx.WriteError("")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(_preflight())

    msg = str(exc_info.value)
    assert "WriteError" in msg
    assert "aiui.app" in msg


# ----- #179: degraded-but-serving must not block, and 503 must carry its hint -----


class _FakeHealthResp:
    """Minimal stand-in for the httpx response `/health` returns."""

    def __init__(self, status_code: int, payload: Any = None, text: str | None = None) -> None:
        self.status_code = status_code
        self._payload = payload
        self.text = text if text is not None else json.dumps(payload)

    def json(self) -> Any:
        if self._payload is None:
            raise ValueError("Expecting value: line 1 column 1 (char 0)")
        return self._payload


def _serve_health(
    monkeypatch: pytest.MonkeyPatch, resp: _FakeHealthResp
) -> None:
    """Answer every GET with `resp`, and short-circuit the wire-compat check
    that `_preflight` runs afterwards so the test isolates the health branch."""

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        return resp

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    monkeypatch.setattr(server, "_wire_checked", True)


def test_preflight_allows_degraded_ready_false(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A full dialog registry is degraded, not down.

    One session accumulating 16 unanswered dialogs used to 503 `/health`, and
    `_preflight` treated any non-200 as fatal — so every *other* session on the
    same companion lost `/render` and `upload` too, even with nothing pending
    of its own. The companion now answers 200 + `ready: false`; preflight must
    carry on.
    """
    _setup_token(monkeypatch, tmp_path)
    _serve_health(
        monkeypatch,
        _FakeHealthResp(
            200,
            {
                "version": "0.5.0",
                "ready": False,
                "reason": "dialog_registry_full",
                "hint": "16 unanswered dialogs are open (cap 16)",
            },
        ),
    )

    asyncio.run(_preflight())  # must not raise


def test_preflight_raises_with_hint_on_webview_unresponsive(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A 503 is reserved for a frozen WebView — and the companion's own `hint`
    goes into the error verbatim, replacing the old `r.text[:200]` blob."""
    _setup_token(monkeypatch, tmp_path)
    hint = "the open dialog window did not answer a liveness ping within 750 ms"
    _serve_health(
        monkeypatch,
        _FakeHealthResp(
            503,
            {"version": "0.5.0", "ready": False, "reason": "webview_unresponsive", "hint": hint},
        ),
    )

    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(_preflight())

    msg = str(exc_info.value)
    assert hint in msg
    assert '{"version"' not in msg, "the raw JSON blob must not be the message"


def test_preflight_still_fatal_on_401(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """Token mismatch stays fatal — the 401 branch is untouched by #179."""
    _setup_token(monkeypatch, tmp_path)
    _serve_health(monkeypatch, _FakeHealthResp(401, {"error": "unauthorized"}))

    with pytest.raises(RuntimeError, match="401"):
        asyncio.run(_preflight())


def test_preflight_rejects_unparseable_200_body(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A 200 that isn't JSON means something other than the companion is
    answering on this port — still fatal."""
    _setup_token(monkeypatch, tmp_path)
    _serve_health(monkeypatch, _FakeHealthResp(200, None, text="<html>nginx</html>"))

    with pytest.raises(RuntimeError, match="unparseable"):
        asyncio.run(_preflight())


def test_aiui_health_returns_body_on_non_200(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """`raise_for_status()` used to discard the composite response — the very
    thing the tool exists to report — and return only the status line."""
    _setup_token(monkeypatch, tmp_path)
    _serve_health(
        monkeypatch,
        _FakeHealthResp(
            503,
            {
                "version": "0.5.0",
                "ready": False,
                "reason": "webview_unresponsive",
                "hint": "close that dialog window on the Mac",
                "pending": 3,
                "oldest_age_secs": 1200,
                "lifecycle_phase": "Serving",
            },
        ),
    )

    result = asyncio.run(aiui_health())

    assert result["ok"] is False
    assert result["ready"] is False
    assert result["reason"] == "webview_unresponsive"
    assert result["hint"] == "close that dialog window on the Mac"
    assert result["pending"] == 3
    assert result["oldest_age_secs"] == 1200
    assert result["lifecycle_phase"] == "Serving"
    # The status is no longer the whole answer, but it stays legible.
    assert result["http_status"] == 503


def test_aiui_health_ok_true_for_degraded_200(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """`ok` tracks "the companion answered", `ready` tracks "it is healthy" —
    a degraded-but-serving companion is `ok: true` / `ready: false`."""
    _setup_token(monkeypatch, tmp_path)
    _serve_health(
        monkeypatch,
        _FakeHealthResp(200, {"ready": False, "reason": "too_many_children"}),
    )

    result = asyncio.run(aiui_health())

    assert result["ok"] is True
    assert result["ready"] is False
    assert result["reason"] == "too_many_children"


def test_aiui_health_reports_non_json_body(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """Dropping `raise_for_status()` must not turn a rogue process on :7777
    into a silent success — a non-JSON body keeps the `{ok: false, error}`
    shape."""
    _setup_token(monkeypatch, tmp_path)
    _serve_health(monkeypatch, _FakeHealthResp(200, None, text="<html>not aiui</html>"))

    result = asyncio.run(aiui_health())

    assert result["ok"] is False
    assert "not aiui" in result["error"]
