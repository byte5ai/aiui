"""Smoke test for the host-agnostic aiui contract on loopback (:7777).

`aiui-mcp` is a standard MCP server: it renders dialogs by POSTing specs to the
companion over `127.0.0.1:7777`. That contract is what makes aiui host-agnostic
— Claude Desktop, OpenAI **Codex**, or any other MCP client that reaches the
companion gets the same `confirm`/`ask`/`form` round-trip. "Codex is officially
supported" (see the README "Using aiui with Codex / ChatGPT" section) stays
honest only while that contract holds.

This test proves it end to end WITHOUT a real companion or a GUI — impossible in
CI anyway (the companion needs a live macOS desktop; it can't render headless).
Instead it stands up a *fake companion*: a tiny loopback HTTP server that
implements exactly the endpoints the bridge calls — unauthenticated `GET /ping`,
bearer-authed `GET /health` and `GET /version`, and `POST /render` — then drives
the REAL tool functions through the REAL httpx path and asserts the wire
round-trip. If the loopback HTTP / auth / render contract breaks, this goes red.

The server binds an ephemeral loopback port (not the fixed 7777, which may be
taken or need the real app) and points the bridge at it — the contract exercised
is identical; only the port number differs.
"""
from __future__ import annotations

import asyncio
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

import pytest

import aiui_mcp.server as server
from aiui_mcp.server import EXPECTED_WIRE_VERSION, ask, confirm, form

# A real aiui token is 64 hex chars; the bridge rejects any other
# shape (#185), so the fixture has to look like the real thing.
TOKEN = "ab1eab1eab1eab1eab1eab1eab1eab1eab1eab1eab1eab1eab1eab1eab1eab1e"

# Canned terminal results per dialog kind, in the companion's wire shape
# ({cancelled, result}). `_format_result` in the bridge flattens these into the
# tool's public return value.
_TERMINAL: dict[str, dict[str, Any]] = {
    "confirm": {"cancelled": False, "result": {"confirmed": True}},
    "ask": {"cancelled": False, "result": {"answers": ["Approve"]}},
    "form": {"cancelled": False, "result": {"values": {"name": "Ada"}}},
}


class _FakeCompanion(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, addr: tuple[str, int]) -> None:
        super().__init__(addr, _Handler)
        self.last_render: dict[str, Any] | None = None
        self.hits: set[str] = set()
        self.force_cancel = False
        # #186: when set, /render answers with the companion's structured
        # invalid_spec rejection instead of a terminal result.
        self.force_invalid_spec: dict[str, Any] | None = None


class _Handler(BaseHTTPRequestHandler):
    server: _FakeCompanion  # type: ignore[assignment]

    def log_message(self, *_: Any) -> None:  # keep pytest output clean
        return

    def _authed(self) -> bool:
        return self.headers.get("Authorization") == f"Bearer {TOKEN}"

    def _send(self, code: int, body: bytes, ctype: str = "application/json") -> None:
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _json(self, code: int, payload: dict[str, Any]) -> None:
        self._send(code, json.dumps(payload).encode(), "application/json")

    def do_GET(self) -> None:  # noqa: N802 — stdlib naming
        if self.path == "/ping":  # unauthenticated readiness probe
            self.server.hits.add("ping")
            self._send(200, b"pong", "text/plain")
            return
        if not self._authed():
            self._json(401, {"error": "unauthorized"})
            return
        if self.path == "/health":
            self.server.hits.add("health")
            self._json(200, {"ready": True, "webview": {}, "dialogs": {}, "children": {}})
            return
        if self.path == "/version":
            self.server.hits.add("version")
            self._json(200, {"version": "fake", "wire_version": EXPECTED_WIRE_VERSION})
            return
        self._json(404, {"error": "not_found"})

    def do_POST(self) -> None:  # noqa: N802 — stdlib naming
        if not self._authed():
            self._json(401, {"error": "unauthorized"})
            return
        n = int(self.headers.get("Content-Length", "0") or "0")
        body = json.loads(self.rfile.read(n) or b"{}")
        if self.path == "/render":
            spec = body.get("spec", {})
            self.server.last_render = {"spec": spec, "async": self.headers.get("x-aiui-async")}
            if self.server.force_invalid_spec is not None:
                self._json(422, self.server.force_invalid_spec)
                return
            if self.server.force_cancel:
                self._json(200, {"cancelled": True})
                return
            self._json(200, _TERMINAL[spec.get("kind")])
            return
        self._json(404, {"error": "not_found"})


@pytest.fixture()
def companion(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> Any:
    srv = _FakeCompanion(("127.0.0.1", 0))
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()

    token_file = tmp_path / "token"
    token_file.write_text(TOKEN)
    monkeypatch.setattr(server, "ENDPOINT", f"http://127.0.0.1:{srv.server_address[1]}")
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)
    monkeypatch.setattr(server, "_wire_checked", False)  # re-run the wire check per test
    try:
        yield srv
    finally:
        srv.shutdown()
        srv.server_close()


def test_confirm_roundtrip_over_loopback(companion: _FakeCompanion) -> None:
    out = asyncio.run(confirm(title="Drop the orders table?", destructive=True))
    assert out == {"cancelled": False, "confirmed": True}
    assert companion.last_render is not None
    assert companion.last_render["spec"]["kind"] == "confirm"
    # the bridge opted into async render and the preflight ran the full gate
    assert companion.last_render["async"] == "1"
    assert {"ping", "health", "version"} <= companion.hits


def test_ask_roundtrip_over_loopback(companion: _FakeCompanion) -> None:
    out = asyncio.run(
        ask(question="Which deploy strategy?", options=[{"label": "Blue"}, {"label": "Green"}])
    )
    assert out == {"cancelled": False, "answers": ["Approve"]}
    assert companion.last_render["spec"]["kind"] == "ask"


def test_ask_without_allow_other_defaults_to_false(companion: _FakeCompanion) -> None:
    """#203: the free-text box is opt-in on BOTH bridges. This bridge used to
    default it on, so identical agent code showed the user an "Other answer"
    field over the tunnel and not on the Mac — and could get back an answer in
    `other` that it never offered."""
    asyncio.run(
        ask(question="Which deploy strategy?", options=[{"label": "Blue"}, {"label": "Green"}])
    )
    assert companion.last_render["spec"]["allowOther"] is False


def test_ask_allow_other_true_is_forwarded(companion: _FakeCompanion) -> None:
    """The flip is a default, not a removal — an agent that asks for the
    free-text fallback still gets it, so the fix can't be over-corrected."""
    asyncio.run(
        ask(
            question="Which deploy strategy?",
            options=[{"label": "Blue"}, {"label": "Green"}],
            allow_other=True,
        )
    )
    assert companion.last_render["spec"]["allowOther"] is True


def test_form_roundtrip_over_loopback(companion: _FakeCompanion) -> None:
    out = asyncio.run(
        form(title="New user", fields=[{"kind": "text", "name": "name", "label": "Name"}])
    )
    assert out == {"cancelled": False, "values": {"name": "Ada"}}
    assert companion.last_render["spec"]["kind"] == "form"


def test_cancel_maps_to_cancelled(companion: _FakeCompanion) -> None:
    companion.force_cancel = True
    out = asyncio.run(confirm(title="Proceed?"))
    assert out == {"cancelled": True}


def test_wrong_token_is_rejected(
    companion: _FakeCompanion, monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """Auth is enforced end to end: a bridge holding the wrong token is turned
    away at the companion's 401, surfaced as an actionable tool error."""
    bad = tmp_path / "bad-token"
    bad.write_text("badc0ffebadc0ffebadc0ffebadc0ffebadc0ffebadc0ffebadc0ffebadc0ffe")
    monkeypatch.setattr(server, "TOKEN_PATH", bad)
    with pytest.raises(RuntimeError) as exc:
        asyncio.run(confirm(title="Proceed?"))
    assert "401" in str(exc.value) or "token" in str(exc.value).lower()



def test_invalid_spec_422_reaches_the_agent_with_its_reason(companion: Any) -> None:
    """#186: the companion rejects a bad spec with `{error, detail, hint}`.

    `raise_for_status()` threw that body away, so a bridge-served agent got a
    bare `HTTPStatusError: 422` and no idea what was wrong with its spec —
    while a local Rust-bridge agent got the full explanation. The reason must
    survive the bridge, or the rejection is useless to the remote half of the
    userbase.
    """
    companion.force_invalid_spec = {
        "error": "invalid_spec",
        "detail": "form field 'pat' has kind 'secret' but no 'target'",
        "hint": "A `secret` field is write-only and must carry a `target`.",
    }
    with pytest.raises(RuntimeError) as exc:
        asyncio.run(form(title="Creds", fields=[{"kind": "secret", "name": "pat"}]))
    msg = str(exc.value)
    assert "invalid_spec" in msg
    assert "'secret' but no 'target'" in msg, f"detail must reach the agent: {msg}"
    assert "write-only" in msg, f"hint must reach the agent: {msg}"


def test_a_422_without_a_body_still_explains_itself(companion: Any) -> None:
    """A companion too old to send `detail`/`hint` must not degrade into a
    bare status error either."""
    companion.force_invalid_spec = {}
    with pytest.raises(RuntimeError) as exc:
        asyncio.run(form(title="x", fields=[]))
    assert "rejected the dialog spec" in str(exc.value)
