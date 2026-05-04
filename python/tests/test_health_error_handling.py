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
"""
from __future__ import annotations

import asyncio
from typing import Any

import httpx
import pytest

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
    token_file.write_text("dummy-token-for-tests")
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
