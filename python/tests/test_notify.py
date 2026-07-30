"""Tests for the `notify` tool (#17) — fire-and-forget native macOS
notification. Unlike confirm/ask/form/gallery this never goes through
`_post_render` (no dialog window, no async-poll dance): it POSTs
`/notify` on the companion and returns the parsed response directly.

These mock `httpx.AsyncClient.post` so they run without a live companion,
mirroring the pattern in test_wire_compat.py / test_health_error_handling.py.
"""
from __future__ import annotations

import asyncio
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import notify


class _FakeResp:
    def __init__(self, status_code: int, payload: dict[str, Any], text: str = "") -> None:
        self.status_code = status_code
        self._payload = payload
        self.text = text or str(payload)

    def json(self) -> dict[str, Any]:
        return self._payload

    def raise_for_status(self) -> None:
        if self.status_code >= 400:
            raise httpx.HTTPStatusError(
                f"http {self.status_code}", request=None, response=None  # type: ignore[arg-type]
            )


def _setup_token(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> None:
    token_file = tmp_path / "token"
    token_file.write_text("dummy-token-for-tests")
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)


def test_notify_success_posts_expected_body_and_returns_ok(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _setup_token(monkeypatch, tmp_path)
    seen: dict[str, Any] = {}

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        seen["url"] = url
        seen["json"] = kwargs.get("json")
        seen["headers"] = kwargs.get("headers")
        return _FakeResp(200, {"ok": True})

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    result = asyncio.run(
        notify(title="Deploy finished", body="All green", subtitle="CI", sound="default")
    )

    assert result == {"ok": True}
    assert seen["url"] == f"{server.ENDPOINT}/notify"
    assert seen["json"] == {
        "title": "Deploy finished",
        "body": "All green",
        "subtitle": "CI",
        "sound": "default",
    }
    assert seen["headers"]["Authorization"] == "Bearer dummy-token-for-tests"


def test_notify_omits_optional_fields_as_none(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """subtitle/sound default to None and are still sent (as null) — the
    companion treats them as optional, so no special-casing needed here."""
    _setup_token(monkeypatch, tmp_path)
    seen: dict[str, Any] = {}

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        seen["json"] = kwargs.get("json")
        return _FakeResp(200, {"ok": True})

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    result = asyncio.run(notify(title="Done", body="Task complete"))

    assert result == {"ok": True}
    assert seen["json"] == {
        "title": "Done",
        "body": "Task complete",
        "subtitle": None,
        "sound": None,
    }


def test_notify_companion_reports_permission_denied_as_ok_false(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A denied OS notification permission is an expected outcome (per the
    tool's docstring), not a tool error — {ok: false, error} passes through."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        return _FakeResp(200, {"ok": False, "error": "permission denied"})

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    result = asyncio.run(notify(title="Done", body="Task complete"))

    assert result == {"ok": False, "error": "permission denied"}


def test_notify_422_invalid_request_raises_with_detail(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """The companion rejects an empty title with a structured 422 — that
    detail must reach the agent so it can fix the call, not a bare status."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        return _FakeResp(
            422, {"error": "invalid_request", "detail": "title must not be empty"}
        )

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(notify(title="", body="whatever"))
    assert "title must not be empty" in str(exc_info.value)


def test_notify_connect_error_surfaces_actionable_message(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A transport failure (companion down) must not leak a bare httpx
    exception — same defensive wrapping as update_tool/version_tool."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        raise httpx.ConnectError("companion down")

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(notify(title="Done", body="Task complete"))
    assert "aiui /notify failed" in str(exc_info.value)
    assert "aiui_health" in str(exc_info.value)


def test_notify_5xx_raises_wrapped_error_not_bare_status_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """A non-422 error status must not bubble up as a raw
    httpx.HTTPStatusError — it should be wrapped like every other failure
    mode this tool handles."""
    _setup_token(monkeypatch, tmp_path)

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        return _FakeResp(500, {"error": "internal"})

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(notify(title="Done", body="Task complete"))
    assert "aiui /notify failed" in str(exc_info.value)
