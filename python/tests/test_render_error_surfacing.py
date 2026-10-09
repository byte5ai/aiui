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

    async def no_media(*args: Any, **kwargs: Any) -> list[str]:
        return []

    monkeypatch.setattr(server, "_wait_for_aiui", noop)
    monkeypatch.setattr(server, "_preflight", noop)
    # These return `list[str]` of media warnings — the render flow does
    # `media_warnings += _upload_local_audios(...)`, so the mock must honour
    # the list contract, not return None.
    monkeypatch.setattr(server, "_upload_local_videos", no_media)
    monkeypatch.setattr(server, "_upload_local_audios", no_media)

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


@pytest.mark.parametrize(
    ("status", "content", "needle"),
    [
        (202, b"<html>proxy page</html>", "unreadable body"),
        (200, b"[]", "non-object body"),
    ],
)
def test_unreadable_render_answer_raises_a_named_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any, status: int, content: bytes, needle: str
) -> None:
    """E-03: `first = r.json()` let a JSONDecodeError (or, for a JSON list, an
    AttributeError on `.get`) escape `_post_render` raw."""
    resp = httpx.Response(
        status, content=content, request=httpx.Request("POST", "http://127.0.0.1:7777/render")
    )
    _setup(monkeypatch, tmp_path, resp)  # type: ignore[arg-type]
    with pytest.raises(RuntimeError) as exc_info:
        _render({"kind": "confirm", "title": "ok?"})
    assert needle in str(exc_info.value)


def test_render_file_io_runs_off_the_event_loop(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """E-08: inlining local images, resolving target paths and performing the
    target writes are blocking file I/O. Run on the event loop they kept it
    from reading stdin — an Esc or a `ping` waited behind a batch of images."""

    class _Terminal:
        status_code = 200

        def json(self) -> dict[str, Any]:
            return {"cancelled": False, "result": {"values": {"name": "Ada"}}}

    _setup(monkeypatch, tmp_path, _Terminal())  # type: ignore[arg-type]
    on_loop: dict[str, bool] = {}

    def recorder(name: str, real: Any) -> Any:
        def wrapped(*args: Any) -> Any:
            try:
                asyncio.get_running_loop()
                on_loop[name] = True
            except RuntimeError:
                on_loop[name] = False
            return real(*args)

        return wrapped

    for name in ("_resolve_local_paths", "_annotate_target_paths", "_apply_target_writes"):
        monkeypatch.setattr(server, name, recorder(name, getattr(server, name)))

    out = _render({"kind": "form", "title": "x", "fields": [{"kind": "text", "name": "name"}]})
    assert out["cancelled"] is False
    assert on_loop == {
        "_resolve_local_paths": False,
        "_annotate_target_paths": False,
        "_apply_target_writes": False,
    }
