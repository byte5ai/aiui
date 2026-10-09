"""The bridge proves the listener is aiui before its token leaves the host (B1-01).

On a remote host the bridge talks to `127.0.0.1:<port>`, the near end of an SSH
reverse-tunnel. While the tunnel is down that port is free, and on a shared host
a co-tenant can bind it and collect `Authorization: Bearer <token>` from the
next request. The bridge therefore first sends a credential-less challenge —
`GET /probe?nonce=<fresh hex>` — and only sends its token once the answer's
`mac = HMAC-SHA256(token, "aiui-probe-v1|nonce|pid|build_sha")` checks out.

Each test stands up a loopback fake that records every request, so "the token
was never sent" is asserted on what actually crossed the socket.
"""

from __future__ import annotations

import asyncio
import hashlib
import hmac
import json
import socket
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import EXPECTED_WIRE_VERSION, _preflight, aiui_health, confirm
from companion_auth import auth_ok

TOKEN = "c0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ff"


class _Listener(ThreadingHTTPServer):
    """Whatever holds the companion port. `mode` decides how it answers the
    challenge:

    - `good`     — the real companion: correct MAC
    - `wrong`    — a squatter guessing: well-formed answer, wrong MAC
    - `no_mac`   — answers like an aiui but without a challenge answer
    - `old`      — a companion older than the challenge: /probe needs auth → 401
    - `missing`  — no /probe route at all → 404
    """

    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, addr: tuple[str, int], mode: str) -> None:
        super().__init__(addr, _Handler)
        self.mode = mode
        self.requests: list[tuple[str, str, str | None]] = []  # (method, path, auth)

    @property
    def auth_headers(self) -> list[str]:
        return [auth for _, _, auth in self.requests if auth is not None]


class _Handler(BaseHTTPRequestHandler):
    server: _Listener  # type: ignore[assignment]

    def log_message(self, *_: Any) -> None:
        return

    def _json(self, code: int, payload: Any) -> None:
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _record(self) -> str | None:
        auth = self.headers.get("Authorization")
        self.server.requests.append((self.command, self.path, auth))
        return auth

    def do_GET(self) -> None:  # noqa: N802 — stdlib naming
        auth = self._record()
        path, _, query = self.path.partition("?")
        if path == "/ping":
            self._json(200, "pong")
            return
        if path == "/probe":
            self._probe(urllib.parse.parse_qs(query).get("nonce", [""])[0], auth)
            return
        if not auth_ok(auth, self.command, self.path, TOKEN):
            self._json(401, {"error": "unauthorized"})
            return
        if path == "/health":
            self._json(200, {"ready": True})
            return
        if path == "/version":
            self._json(200, {"version": "fake", "wire_version": EXPECTED_WIRE_VERSION})
            return
        self._json(404, {"error": "not_found"})

    def _probe(self, nonce: str, auth: str | None) -> None:
        mode = self.server.mode
        if mode == "missing":
            self._json(404, {"error": "not_found"})
            return
        if mode == "old":
            # Pre-challenge companion: /probe always wanted the bearer token.
            self._json(401, {"error": "unauthorized"})
            return
        pid, sha = 31337, "deadbeef"
        key = TOKEN if mode == "good" else "f" * 64
        mac = hmac.new(
            key.encode(), f"aiui-probe-v1|{nonce}|{pid}|{sha}".encode(), hashlib.sha256
        ).hexdigest()
        body: dict[str, Any] = {"aiui": True, "pid": pid, "build_sha": sha, "mac": mac}
        if mode == "no_mac":
            del body["mac"]
        self._json(200, body)


def _start(mode: str, port: int = 0) -> _Listener:
    srv = _Listener(("127.0.0.1", port), mode)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def _stop(srv: _Listener) -> None:
    srv.shutdown()
    srv.server_close()


@pytest.fixture()
def bridge(monkeypatch: pytest.MonkeyPatch, tmp_path: Any) -> Any:
    """Point the bridge at a listener; yields a function that starts one."""
    token_file = tmp_path / "token"
    token_file.write_text(TOKEN)
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)
    monkeypatch.setattr(server, "_wire_checked", False)
    monkeypatch.setattr(server, "COLDSTART_WAIT_S", 2.0)
    monkeypatch.setattr(server, "_LISTENER_VERIFIED", {}, raising=False)
    monkeypatch.delenv("AIUI_ALLOW_UNVERIFIED_COMPANION", raising=False)
    started: list[_Listener] = []

    def start(mode: str, port: int = 0) -> _Listener:
        srv = _start(mode, port)
        started.append(srv)
        monkeypatch.setattr(server, "ENDPOINT", f"http://127.0.0.1:{srv.server_address[1]}")
        return srv

    yield start
    for srv in started:
        try:
            _stop(srv)
        except OSError:
            pass


def test_a_verified_listener_gets_the_token(bridge: Any) -> None:
    """(a) The real companion answers the challenge; only then does the token
    go out — and the challenge itself never carries it."""
    srv = bridge("good")
    asyncio.run(_preflight())
    methods_paths = [(m, p.partition("?")[0]) for m, p, _ in srv.requests]
    assert methods_paths[0] == ("GET", "/probe"), "the challenge comes first"
    probes = [r for r in srv.requests if r[1].startswith("/probe")]
    assert all(auth is None for _, _, auth in probes), "the challenge carries no token"
    nonce = urllib.parse.parse_qs(probes[0][1].partition("?")[2])["nonce"][0]
    assert len(nonce) == 64 and all(c in "0123456789abcdef" for c in nonce)
    health = [a for m, p, a in srv.requests if p == "/health"]
    assert health and all(a.startswith("AIUI-HMAC ") for a in health), health
    # The token itself never reaches the listener, not even the real one.
    assert all(TOKEN not in a for a in srv.auth_headers), srv.auth_headers

    # No cross-request trust: every token-bearing request is directly
    # preceded by its own challenge (Codex review of the 60 s cache).
    asyncio.run(_preflight())
    for i, (_, _path, auth) in enumerate(srv.requests):
        if auth is not None:
            assert i > 0 and srv.requests[i - 1][1].startswith("/probe"), srv.requests


@pytest.mark.parametrize("mode", ["wrong", "no_mac", "missing"])
def test_an_unproven_listener_never_sees_the_token(bridge: Any, mode: str) -> None:
    """(b) A wrong or missing challenge answer: the token is withheld and the
    tool call fails with an error the agent can act on."""
    srv = bridge(mode)
    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(confirm(title="Drop the orders table?"))
    msg = str(exc_info.value)
    assert srv.auth_headers == [], f"token leaked to an unverified listener: {srv.requests}"
    assert "could not prove it is aiui" in msg
    assert "older aiui companion" in msg and "foreign process" in msg
    port = str(srv.server_address[1])
    assert port in msg, "the error names the port"


def test_health_reports_an_unproven_listener_without_sending_the_token(bridge: Any) -> None:
    srv = bridge("wrong")
    out = asyncio.run(aiui_health())
    assert out["ok"] is False
    assert "could not prove it is aiui" in out["error"]
    assert srv.auth_headers == []


def test_an_older_companion_is_refused_unless_opted_in(
    bridge: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    """(c) A pre-challenge companion answers the credential-less probe with
    401. That is indistinguishable from a squatter that answers 401, so the
    token is withheld unless the user opted in — and the error says how."""
    srv = bridge("old")
    with pytest.raises(RuntimeError) as exc_info:
        asyncio.run(_preflight())
    msg = str(exc_info.value)
    assert srv.auth_headers == []
    assert "AIUI_ALLOW_UNVERIFIED_COMPANION=1" in msg
    assert "update aiui" in msg

    monkeypatch.setenv("AIUI_ALLOW_UNVERIFIED_COMPANION", "1")
    asyncio.run(_preflight())
    # Opted in: an older companion only understands the bearer form.
    assert ("GET", "/health", f"Bearer {TOKEN}") in srv.requests


def test_a_port_that_changes_hands_after_an_outage_is_challenged_again(bridge: Any) -> None:
    """The attack itself: aiui held the port and was verified, the tunnel drops
    (requests fail), and a squatter binds the port. The connection error
    forgets the verification, so the squatter gets a challenge — not the token."""
    good = bridge("good")
    asyncio.run(_preflight())
    port = good.server_address[1]
    _stop(good)

    # The tunnel is down: the next call fails to connect…
    out = asyncio.run(aiui_health())
    assert out["ok"] is False

    # …and a squatter takes the free port.
    squatter = bridge("wrong", port)
    with pytest.raises(RuntimeError, match="could not prove it is aiui"):
        asyncio.run(_preflight())
    assert squatter.auth_headers == [], f"token leaked to the squatter: {squatter.requests}"


def test_a_port_taken_over_between_two_calls_never_sees_the_token(bridge: Any) -> None:
    """Codex review: with no failed request in between, an address-level cache
    trusted the port for 60 s across clients — the next client's fresh
    connection then went to the squatter WITH the token. Every token-bearing
    request is now challenged on its own."""
    good = bridge("good")
    asyncio.run(_preflight())
    port = good.server_address[1]
    _stop(good)
    # No call in between: the squatter binds at once.
    squatter = bridge("wrong", port)
    with pytest.raises(RuntimeError, match="could not prove it is aiui"):
        asyncio.run(_preflight())
    assert squatter.auth_headers == [], f"token leaked to the squatter: {squatter.requests}"


def test_a_signature_binds_body_mode_and_companion_process() -> None:
    """Codex review: a signature over method and path alone let a captured
    header carry a substituted body, or drop `x-aiui-async` to get the answer
    back synchronously."""
    req = httpx.Request(
        "POST",
        "http://127.0.0.1:7777/render",
        content=b'{"a": 1}',
        headers={"x-aiui-async": "1"},
    )
    hdr = asyncio.run(server._signed_authorization(TOKEN, req, 4242))
    assert TOKEN not in hdr
    assert "pid=4242" in hdr
    assert auth_ok(hdr, "POST", "/render", TOKEN, body=b'{"a": 1}', async_hdr="1")
    assert not auth_ok(hdr, "POST", "/render", TOKEN, body=b'{"a": 2}', async_hdr="1")
    assert not auth_ok(hdr, "POST", "/render", TOKEN, body=b'{"a": 1}', async_hdr="")


def test_request_mac_matches_the_companion_formula() -> None:
    """The vector `the_signed_request_mac_formula_is_pinned` pins in http.rs."""
    empty = hashlib.sha256(b"").hexdigest()
    got = server._request_mac(
        "k" * 64, "POST", "/render", 1_700_000_000, "ab" * 16, 4242, empty, "1"
    )
    assert got == "69e9c08260c11b26fc5f217fb7bbe0699291522a1c69f53da4095b99b04fea21"


def test_probe_mac_matches_the_companion_formula() -> None:
    """Pinned against an independent computation of the documented formula,
    so the bridge and `http.rs::probe_mac` agree by construction."""
    nonce = "ab" * 16
    expected = hmac.new(
        b"k" * 64, f"aiui-probe-v1|{nonce}|7|abc".encode(), hashlib.sha256
    ).hexdigest()
    assert server._probe_mac("k" * 64, nonce, 7, "abc") == expected


def test_the_fake_port_is_really_free_after_stop() -> None:
    """Guard for the outage test above: a stopped listener refuses."""
    srv = _start("good")
    port = srv.server_address[1]
    _stop(srv)
    with socket.socket() as s, pytest.raises(OSError):
        s.connect(("127.0.0.1", port))
