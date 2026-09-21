"""Tests for the `update` MCP tool (#197).

The tool is a thin pass-through of the companion's `/update` response, and
that is precisely what must be pinned: two of the outcomes are *not* errors
and must not be turned into one.

  * `updated: false` + `note: "dialog in flight — update deferred"` — the
    companion refuses to restart out from under a dialog the user is filling
    in. Before #197 that outcome did not exist; the companion just installed.
  * On Windows the companion now answers *before* handing the installer to
    `ShellExecuteW` + `process::exit(0)`. Previously the response was built
    after that call, so it was never flushed and this tool raised
    `aiui /update failed at …` instead of returning the version delta.

A dropped connection still has to surface as a RuntimeError — that path is
how a genuinely unreachable companion is reported.
"""

from __future__ import annotations

import asyncio
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import update_tool


class _FakeResp:
    def __init__(self, payload: dict[str, Any]) -> None:
        self._payload = payload

    def raise_for_status(self) -> None:  # all fakes are 200
        return None

    def json(self) -> dict[str, Any]:
        return self._payload


def _setup_token(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
    token_file = tmp_path / "token"
    token_file.write_text("de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57")
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)


def _respond_with(monkeypatch: pytest.MonkeyPatch, payload: dict[str, Any]) -> None:
    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        assert url.endswith("/update")
        return _FakeResp(payload)

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)


def _call() -> dict[str, Any]:
    return asyncio.run(update_tool())


def test_deferred_because_a_dialog_is_open_is_a_result_not_an_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    _respond_with(
        monkeypatch,
        {
            "updated": False,
            "current": "0.10.1",
            "available": "0.10.2",
            "error": None,
            "note": "dialog in flight — update deferred",
        },
    )

    result = _call()

    assert result["updated"] is False
    assert result["error"] is None
    assert result["note"] == "dialog in flight — update deferred"
    # The agent still learns which version it *would* have installed, so it
    # can tell the user what they are waiting for.
    assert result["available"] == "0.10.2"


def test_windows_install_returns_the_version_delta_before_the_process_exits(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    _respond_with(
        monkeypatch,
        {
            "updated": True,
            "current": "0.10.1",
            "available": "0.10.2",
            "error": None,
            "note": "installer launched — aiui restarts into the new version",
        },
    )

    result = _call()

    assert result["updated"] is True
    assert (result["current"], result["available"]) == ("0.10.1", "0.10.2")


def test_already_on_latest_is_unchanged(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
    _setup_token(monkeypatch, tmp_path)
    _respond_with(
        monkeypatch,
        {
            "updated": False,
            "current": "0.10.1",
            "available": None,
            "error": None,
            "note": "already on latest",
        },
    )

    assert _call()["note"] == "already on latest"


def test_unreachable_companion_still_raises(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
    _setup_token(monkeypatch, tmp_path)

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.ConnectError("connection refused")

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    with pytest.raises(RuntimeError, match="aiui /update failed"):
        _call()
