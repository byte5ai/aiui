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

The companion a current bridge actually talks to answers `POST /render` with
`202 {id, ttl_secs}` and hands the result over `GET /render/{id}` — the fake
honours `x-aiui-async` so the smoke tests exercise that production path (#202).
`sync_mode` keeps the legacy synchronous 200 covered, because an old companion
really does answer that way and the bridge must keep handling it.
"""
from __future__ import annotations

import asyncio
import json
import socket
import threading
import uuid
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
    """A current companion, async by default (#202).

    `POST /render` with `x-aiui-async: 1` registers a dialog and answers
    `202 {id, ttl_secs}`; `GET /render/{id}` then serves one `{"pending": true}`
    before the terminal body, mirroring the real server's poll window. The
    knobs let a test drive the failure paths the production path has:

    - `sync_mode`     — answer the legacy synchronous 200 instead (old companion)
    - `force_cancel`  — terminal body is a cancel, optionally with `cancel_reason`
    - `drop_polls`    — slam the connection shut on the next N polls
    - `omit_render_id`— answer 202 with no `id` (companion bug)
    - `force_invalid_spec` — 422 with the structured `{error, detail, hint}`
    """

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
        # --- async-render state (#202) ---
        self.sync_mode = False
        self.cancel_reason: str | None = None
        self.drop_polls = 0
        self.omit_render_id = False
        self.terminal_override: dict[str, Any] | None = None
        self.render_posts = 0
        self.poll_gets = 0
        # id → {"terminal": <body>, "served_pending": bool}
        self.pending: dict[str, dict[str, Any]] = {}

    def _terminal_body(self, kind: str) -> dict[str, Any]:
        if self.force_cancel:
            body: dict[str, Any] = {"cancelled": True, "result": None}
            if self.cancel_reason:
                body["reason"] = self.cancel_reason
            return body
        if self.terminal_override is not None:
            return self.terminal_override
        return _TERMINAL[kind]


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
        if self.path.startswith("/render/"):
            self._do_poll(self.path[len("/render/"):])
            return
        self._json(404, {"error": "not_found"})

    def _do_poll(self, render_id: str) -> None:
        """`GET /render/{id}` — the async result channel (#202).

        Mirrors `http.rs`: one `{"pending": true}` for the poll window, then
        the terminal body; an unknown id is a 404 the bridge must NOT retry.
        """
        self.server.poll_gets += 1
        if self.server.drop_polls > 0:
            # Slam the socket shut mid-request: the bridge sees a transport
            # error (httpx ReadError / RemoteProtocolError), exactly what an
            # SSH reverse-tunnel re-establish looks like.
            self.server.drop_polls -= 1
            self.close_connection = True
            try:
                self.connection.close()
            except OSError:
                pass
            return
        slot = self.server.pending.get(render_id)
        if slot is None:
            self._json(404, {"error": "unknown_render_id"})
            return
        if not slot["served_pending"]:
            slot["served_pending"] = True
            self._json(200, {"pending": True})
            return
        self._json(200, slot["terminal"])

    def do_POST(self) -> None:  # noqa: N802 — stdlib naming
        if not self._authed():
            self._json(401, {"error": "unauthorized"})
            return
        n = int(self.headers.get("Content-Length", "0") or "0")
        body = json.loads(self.rfile.read(n) or b"{}")
        if self.path == "/render":
            spec = body.get("spec", {})
            self.server.render_posts += 1
            self.server.last_render = {
                "spec": spec,
                "async": self.headers.get("x-aiui-async"),
                "body": body,
            }
            if self.server.force_invalid_spec is not None:
                self._json(422, self.server.force_invalid_spec)
                return
            terminal = self.server._terminal_body(spec.get("kind"))
            # A current companion registers the dialog and answers 202; only a
            # companion too old to know the header answers synchronously.
            if self.headers.get("x-aiui-async") == "1" and not self.server.sync_mode:
                if self.server.omit_render_id:
                    self._json(202, {"ttl_secs": 7200})
                    return
                render_id = uuid.uuid4().hex
                self.server.pending[render_id] = {
                    "terminal": terminal,
                    "served_pending": False,
                }
                self._json(202, {"id": render_id, "ttl_secs": 7200})
                return
            self._json(200, terminal)
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
    # Keep the retry budget's semantics but not its wall-clock: the give-up
    # path would otherwise spend 5 s sleeping between dropped polls.
    monkeypatch.setattr(server, "ASYNC_POLL_RETRY_BACKOFF_S", 0.01)
    monkeypatch.setattr(server, "ASYNC_POLL_MIN_INTERVAL_S", 0.0)
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


def test_form_roundtrip_over_loopback(companion: _FakeCompanion) -> None:
    out = asyncio.run(
        form(title="New user", fields=[{"kind": "text", "name": "name", "label": "Name"}])
    )
    assert out == {"cancelled": False, "values": {"name": "Ada"}}
    assert companion.last_render["spec"]["kind"] == "form"


def test_cancel_keeps_documented_shape(companion: _FakeCompanion) -> None:
    """#202: a cancel must still carry the tool's documented keys.

    `_format_result` answered a bare `{"cancelled": True}`, contradicting
    `confirm`'s own docstring (`{cancelled, confirmed}`) and the Rust bridge,
    whose `format_confirm_result` always emits both. An agent reading
    `result["confirmed"]` therefore worked on a Mac-local session and raised
    `KeyError` only on a remote — the worst place for a contract to fork.
    """
    companion.force_cancel = True
    assert asyncio.run(confirm(title="Proceed?")) == {
        "cancelled": True,
        "confirmed": False,
    }
    assert asyncio.run(ask(question="Which?", options=[{"label": "A"}])) == {
        "cancelled": True,
        "answers": [],
    }
    assert asyncio.run(
        form(title="New user", fields=[{"kind": "text", "name": "name", "label": "Name"}])
    ) == {"cancelled": True, "values": {}}


def test_ttl_expired_reason_reaches_agent(companion: _FakeCompanion) -> None:
    """The companion distinguishes "the user said no" from "we gave up"; the
    bridge must not flatten them. An agent that reports "you declined the
    migration" after a 2 h TTL expiry is reporting a decision nobody made."""
    companion.force_cancel = True
    companion.cancel_reason = "ttl_expired"
    assert asyncio.run(confirm(title="Drop the orders table?")) == {
        "cancelled": True,
        "confirmed": False,
        "reason": "ttl_expired",
    }
    # …and a plain user cancel must not acquire an invented reason.
    companion.cancel_reason = None
    out = asyncio.run(confirm(title="Proceed?"))
    assert "reason" not in out, out


def test_async_render_roundtrip_over_loopback(companion: _FakeCompanion) -> None:
    """The production path end to end: 202 → poll → terminal.

    Every smoke test used to exercise the legacy synchronous fallback, because
    the fake ignored the `x-aiui-async` header it asserted was sent — so the
    branch a current companion actually takes was never run (#202).
    """
    out = asyncio.run(confirm(title="Drop the orders table?", destructive=True))
    assert out == {"cancelled": False, "confirmed": True}
    assert companion.last_render["async"] == "1"
    assert companion.render_posts == 1, "exactly one dialog window was opened"
    assert companion.poll_gets >= 2, "one pending poll, then the terminal one"


def test_legacy_sync_companion_still_works(companion: _FakeCompanion) -> None:
    """An old companion ignores the header and answers 200 with the terminal
    body. That fallback must stay alive, or upgrading the bridge first breaks
    the pairing."""
    companion.sync_mode = True
    assert asyncio.run(confirm(title="Proceed?")) == {"cancelled": False, "confirmed": True}
    assert companion.poll_gets == 0, "a sync companion is never polled"


def test_render_body_carries_session_origin(companion: _FakeCompanion) -> None:
    """`session_origin` is the field the frontend (`DialogShell.svelte`) reads
    to decide whether it or the bridge performs `target` writes. If it were
    ever dropped, a secret would be written on the Mac instead of the agent
    host — silently, and no test would have failed."""
    asyncio.run(confirm(title="Proceed?", session="deploy-bot"))
    body = companion.last_render["body"]
    assert body["session_origin"] == socket.gethostname()
    assert body["session"] == "deploy-bot"


def test_poll_survives_dropped_connection(companion: _FakeCompanion) -> None:
    """#202: the async design exists so a transport blip cannot cost the user's
    think-time. The dialog is still on screen; aborting abandons an answered
    window and makes the agent's retry open a *second* one."""
    companion.drop_polls = 1
    out = asyncio.run(confirm(title="Drop the orders table?"))
    assert out == {"cancelled": False, "confirmed": True}
    assert companion.render_posts == 1, "the blip must never re-POST /render"


def test_poll_gives_up_after_consecutive_failures(companion: _FakeCompanion) -> None:
    """The retry is bounded, and the give-up message must be diagnosable —
    httpx leaves `str(e)` empty for this error class, which is how the
    `error: ""` class of bug used to reach the agent."""
    companion.drop_polls = 99
    with pytest.raises(RuntimeError) as exc:
        asyncio.run(confirm(title="Proceed?"))
    msg = str(exc.value)
    assert msg.strip(), "the give-up error must never be empty"
    render_id = next(iter(companion.pending))
    assert render_id in msg, f"the render id must be in the message: {msg}"
    assert companion.render_posts == 1, "give-up must not open a second window"


def test_target_write_lands_on_bridge_host_after_async_render(
    companion: _FakeCompanion, tmp_path: Any
) -> None:
    """`_apply_target_writes` must still run after the *async* render.

    It was only ever tested as a pure helper, so nothing pinned it being wired
    into `_post_render` on the 202 path. This is the secret-handling path: the
    value must land in a file on THIS host and never come back to the agent.
    """
    dest = tmp_path / "pat"
    companion.terminal_override = {
        "cancelled": False,
        "result": {"values": {"pat": "ghp_s3cr3t"}},
    }
    out = asyncio.run(
        form(
            title="Creds",
            fields=[
                {
                    "kind": "secret",
                    "name": "pat",
                    "label": "PAT",
                    "target": {"mode": "create", "path": str(dest)},
                }
            ],
        )
    )
    assert companion.poll_gets >= 2, "this must have gone through the 202 path"
    assert dest.read_text() == "ghp_s3cr3t"
    outcome = out["values"]["pat"]
    assert outcome["written"] is True, outcome
    assert "ghp_s3cr3t" not in json.dumps(out), f"secret leaked to the agent: {out}"


def test_async_202_without_id_raises(companion: _FakeCompanion) -> None:
    """A 202 with nothing to poll is a companion bug; the agent must get a
    named RuntimeError, not a KeyError/TypeError from deep in the bridge."""
    companion.omit_render_id = True
    with pytest.raises(RuntimeError) as exc:
        asyncio.run(confirm(title="Proceed?"))
    assert "id" in str(exc.value)


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
