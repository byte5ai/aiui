"""What a fake companion accepts as authentication, mirroring http.rs.

The bridge never sends its token: requests carry `AIUI-HMAC ts,nonce,mac`
(see `server._signed_authorization`). Fake companions in the tests verify
that signature exactly like `signed_auth` in the companion does, and keep
accepting the bearer form the local Rust bridge and older bridges use.
"""

from __future__ import annotations

import hashlib
import hmac

from aiui_mcp import server


def auth_ok(
    header: str | None,
    method: str,
    path_query: str,
    token: str,
    body: bytes | None = None,
    async_hdr: str = "",
) -> bool:
    """`body`, when given, must match the signed digest (the companion checks
    it once the body is read)."""
    if header is None:
        return False
    if header == f"Bearer {token}":
        return True
    if not header.startswith("AIUI-HMAC "):
        return False
    try:
        params = dict(p.split("=", 1) for p in header[len("AIUI-HMAC ") :].split(","))
        ts, nonce, pid = int(params["ts"]), params["nonce"], int(params["pid"])
        bd, mac = params["bd"], params["mac"]
    except (KeyError, ValueError):
        return False
    want = server._request_mac(token, method, path_query, ts, nonce, pid, bd, async_hdr)
    if not hmac.compare_digest(want, mac):
        return False
    return body is None or hashlib.sha256(body).hexdigest() == bd
