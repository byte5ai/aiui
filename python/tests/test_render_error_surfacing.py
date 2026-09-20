"""Tests for how `_post_render` surfaces the companion's structured refusals.

#178: the companion answers a malformed spec with `422 {error, detail, hint}`
and an oversized one with `413 {error: "spec_too_large", detail, hint}`. Both
bodies exist so the agent can fix the call — `raise_for_status()` would throw
them away and leave a bare `httpx.HTTPStatusError` with a status code and
nothing else.
"""
from __future__ import annotations

import asyncio
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import _post_render


class _FakeResp:
    def __init__(self, payload: dict[str, Any], status: int) -> None:
        self._payload = payload
        self.status_code = status

    def raise_for_status(self) -> None:
        raise AssertionError("the status arm must handle this before raise_for_status")

    def json(self) -> dict[str, Any]:
        return self._payload


def _setup(monkeypatch: pytest.MonkeyPatch, tmp_path: Any, resp: _FakeResp) -> None:
    token_file = tmp_path / "token"
    token_file.write_text("de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57de1e7e57")
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)

    async def noop(*args: Any, **kwargs: Any) -> None:
        return None

    monkeypatch.setattr(server, "_wait_for_aiui", noop)
    monkeypatch.setattr(server, "_preflight", noop)
    monkeypatch.setattr(server, "_upload_local_videos", noop)
    monkeypatch.setattr(server, "_upload_local_audios", noop)

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        return resp

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)


def _render(spec: dict[str, Any]) -> dict[str, Any]:
    return asyncio.run(_post_render(spec))


def test_422_raises_runtime_error_carrying_detail_and_hint(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup(
        monkeypatch,
        tmp_path,
        _FakeResp(
            {
                "error": "invalid_spec",
                "detail": "gallery item has a duplicate 'value': \"a\"",
                "hint": "Each gallery item's 'value' must be unique.",
            },
            status=422,
        ),
    )
    with pytest.raises(RuntimeError) as exc_info:
        _render({"kind": "gallery", "items": []})
    msg = str(exc_info.value)
    assert "invalid_spec" in msg
    assert "duplicate" in msg
    assert "must be unique" in msg  # the hint survives, not just the status


def test_413_raises_runtime_error_carrying_the_spec_too_large_hint(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup(
        monkeypatch,
        tmp_path,
        _FakeResp(
            {
                "error": "spec_too_large",
                "detail": "dialog spec is 60000000 bytes (max 50331648)",
                "hint": (
                    "Inlined images dominate the spec — shrink the image, send fewer "
                    "at once, or pass an http(s):// src instead of a local path."
                ),
            },
            status=413,
        ),
    )
    with pytest.raises(RuntimeError) as exc_info:
        _render({"kind": "confirm", "title": "ok?"})
    msg = str(exc_info.value)
    assert "spec_too_large" in msg
    assert "Inlined images" in msg


def test_413_without_a_json_body_still_explains_itself(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A 413 from somewhere other than our handler (an older companion, a
    proxy) has no JSON body — the agent must still get a sentence, not a
    traceback."""

    class _TextResp(_FakeResp):
        def json(self) -> dict[str, Any]:
            raise ValueError("not json")

    _setup(monkeypatch, tmp_path, _TextResp({}, status=413))
    with pytest.raises(RuntimeError) as exc_info:
        _render({"kind": "confirm", "title": "ok?"})
    assert "too large" in str(exc_info.value)
