"""A host that quits mid-dialog takes the dialog with it (E-07).

When Claude Code (or any MCP host) exits, the bridge's stdin closes. On mcp
1.26 nothing cancelled the in-flight tool handler: the bridge kept polling —
refreshing the slot, so the companion's abandonment reaper never fired — and
the dialog stayed on the user's desktop for up to the 2 h TTL. mcp 1.27 cancels
in-flight handlers on transport close, which is why it is the declared floor;
`_poll_render` then retracts the dialog, and `_lifespan` awaits whatever
retract is still in flight before the event loop is torn down.

This runs the real `aiui-mcp` entry point in a subprocess against a loopback
fake companion, opens a `confirm`, closes stdin mid-poll, and asserts the
bridge exits and completed the DELETE round trip first. Against mcp 1.26 it
fails (the bridge never exits), so the `lowest-direct` CI leg now proves the
floor instead of assuming it. The fake answers the DELETE only after a delay,
so a request that is merely *started* and then cancelled at teardown does not
count — the bridge's own "cancelled render … (http 204)" log line is the proof
that the round trip finished.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import queue
import subprocess
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from aiui_mcp.server import EXPECTED_WIRE_VERSION

TOKEN = "5ca1ab1e" * 8
RENDER_ID = "host-exit-render"


class _Companion(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self) -> None:
        super().__init__(("127.0.0.1", 0), _Handler)
        self.polled = threading.Event()
        self.deletes: list[str] = []


class _Handler(BaseHTTPRequestHandler):
    server: _Companion  # type: ignore[assignment]

    def log_message(self, *_: Any) -> None:
        return

    def _json(self, code: int, payload: Any) -> None:
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _authed(self) -> bool:
        return self.headers.get("Authorization") == f"Bearer {TOKEN}"

    def do_GET(self) -> None:  # noqa: N802 — stdlib naming
        path, _, query = self.path.partition("?")
        if path == "/ping":
            self._json(200, "pong")
            return
        if path == "/probe":
            nonce = urllib.parse.parse_qs(query).get("nonce", [""])[0]
            msg = f"aiui-probe-v1|{nonce}|1|sha".encode()
            mac = hmac.new(TOKEN.encode(), msg, hashlib.sha256).hexdigest()
            self._json(200, {"aiui": True, "pid": 1, "build_sha": "sha", "mac": mac})
            return
        if not self._authed():
            self._json(401, {"error": "unauthorized"})
            return
        if path == "/health":
            self._json(200, {"ready": True})
        elif path == "/version":
            self._json(200, {"version": "fake", "wire_version": EXPECTED_WIRE_VERSION})
        elif path == f"/render/{RENDER_ID}":
            self.server.polled.set()
            time.sleep(0.2)  # a short stand-in for the companion's poll window
            self._json(200, {"pending": True, "id": RENDER_ID})
        else:
            self._json(404, {"error": "not_found"})

    def do_POST(self) -> None:  # noqa: N802 — stdlib naming
        n = int(self.headers.get("Content-Length", "0") or "0")
        self.rfile.read(n)
        if self.path == "/render" and self._authed():
            self._json(202, {"id": RENDER_ID, "ttl_secs": 7200})
            return
        self._json(404, {"error": "not_found"})

    def do_DELETE(self) -> None:  # noqa: N802 — stdlib naming
        if self._authed() and self.path == f"/render/{RENDER_ID}":
            # Slow on purpose: a DELETE that loop teardown cancels mid-flight
            # must not pass for one that completed.
            time.sleep(0.5)
            self.server.deletes.append(RENDER_ID)
        self.send_response(204)
        self.send_header("Content-Length", "0")
        self.end_headers()


def _reader(stream: Any, out: queue.Queue[str]) -> None:
    for line in iter(stream.readline, ""):
        out.put(line)


def _send(proc: subprocess.Popen[str], msg: dict[str, Any]) -> None:
    assert proc.stdin is not None
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()


def test_host_exit_mid_dialog_retracts_the_dialog(tmp_path: Path) -> None:
    companion = _Companion()
    threading.Thread(target=companion.serve_forever, daemon=True).start()
    token_file = tmp_path / "token"
    token_file.write_text(TOKEN)
    stderr_path = tmp_path / "bridge.log"
    env = {
        **os.environ,
        "AIUI_ENDPOINT": f"http://127.0.0.1:{companion.server_address[1]}",
        "AIUI_TOKEN_PATH": str(token_file),
        "AIUI_COLDSTART_WAIT_S": "5",
        "AIUI_LOG_LEVEL": "INFO",
        "PYTHONUNBUFFERED": "1",
    }
    with stderr_path.open("w", encoding="utf-8") as stderr:
        proc = subprocess.Popen(
            [sys.executable, "-m", "aiui_mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr,
            text=True,
            encoding="utf-8",
            env=env,
        )
        try:
            lines: queue.Queue[str] = queue.Queue()
            threading.Thread(target=_reader, args=(proc.stdout, lines), daemon=True).start()
            _send(
                proc,
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "clientInfo": {"name": "host-exit-test", "version": "0"},
                    },
                },
            )
            init = json.loads(lines.get(timeout=30))
            assert init.get("id") == 1 and "result" in init, init
            _send(proc, {"jsonrpc": "2.0", "method": "notifications/initialized"})
            _send(
                proc,
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": "confirm", "arguments": {"title": "Proceed?"}},
                },
            )
            assert companion.polled.wait(timeout=30), "the bridge never started polling"

            # The host quits: stdin closes while the dialog is still open.
            assert proc.stdin is not None
            proc.stdin.close()
            proc.wait(timeout=30)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            companion.shutdown()
            companion.server_close()

    log = stderr_path.read_text(encoding="utf-8")
    assert companion.deletes == [RENDER_ID], f"expected exactly one DELETE\n{log}"
    assert f"cancelled render {RENDER_ID} (http 204)" in log, (
        f"the DELETE was started but not completed before exit\n{log}"
    )
