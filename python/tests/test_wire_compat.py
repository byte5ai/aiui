"""Tests for the cooperative version floor (Step 2).

The Mac companion no longer kills this bridge to force a version. Instead both
sides carry a `wire_version`; on a hard mismatch the bridge surfaces a
structured "restart this session" tool error and otherwise tolerates ordinary
app-version skew. These cover `_check_wire_compat`.
"""
from __future__ import annotations

import asyncio
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import EXPECTED_WIRE_VERSION, _check_wire_compat


class _FakeResp:
    def __init__(self, payload: dict[str, Any]) -> None:
        self._payload = payload

    def raise_for_status(self) -> None:  # all fakes are 200
        return None

    def json(self) -> dict[str, Any]:
        return self._payload


def _setup_token(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
    token_file = tmp_path / "token"
    token_file.write_text("dummy-token-for-tests")
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)


def _run_check() -> None:
    async def run() -> None:
        async with httpx.AsyncClient() as client:
            await _check_wire_compat(client)

    asyncio.run(run())


def test_matching_wire_version_passes_and_memoises(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    monkeypatch.setattr(server, "_wire_checked", False)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        return _FakeResp({"wire_version": EXPECTED_WIRE_VERSION})

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    _run_check()  # must not raise
    assert server._wire_checked is True


def test_mismatched_wire_version_raises_structured_error_and_does_not_memoise(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    monkeypatch.setattr(server, "_wire_checked", False)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        return _FakeResp({"wire_version": EXPECTED_WIRE_VERSION + 998})

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    with pytest.raises(RuntimeError) as exc_info:
        _run_check()
    assert "incompatible aiui versions" in str(exc_info.value)
    # A mismatch must NOT be memoised — a later restart of the companion should
    # be able to clear it without restarting the bridge process.
    assert server._wire_checked is False


def test_missing_wire_version_field_is_tolerated(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """An older companion without the field is treated as wire v1 → compatible."""
    _setup_token(monkeypatch, tmp_path)
    monkeypatch.setattr(server, "_wire_checked", False)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        return _FakeResp({"version": "0.4.46"})  # no wire_version

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    _run_check()  # must not raise
    assert server._wire_checked is True


def test_read_error_is_tolerated_not_fatal(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A transient /version read failure must not block rendering."""
    _setup_token(monkeypatch, tmp_path)
    monkeypatch.setattr(server, "_wire_checked", False)

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.ConnectError("companion down")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)
    _run_check()  # must not raise — tolerate skew/transient
    assert server._wire_checked is True
