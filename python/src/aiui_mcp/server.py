"""aiui MCP server — renders native macOS dialogs via the aiui companion.

Topology:

    Claude Code (local or remote) ──stdio──► aiui-mcp (this process)
                                               │ HTTP
                                               ▼
                                     http://127.0.0.1:7777
                                               │  (local, or via SSH reverse-tunnel)
                                               ▼
                                       Mac: aiui.app (Tauri companion)

The aiui token is read from `~/.config/aiui/token` — installed once when
the companion runs on the Mac, and scp'd automatically to each remote host
registered in the companion's settings window.
"""
from __future__ import annotations

import asyncio
import base64
import importlib.metadata
import importlib.resources as resources
import logging
import mimetypes
import os
import socket
import sys
import tempfile
import time
import urllib.parse
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import httpx
from mcp.server.fastmcp import Context, FastMCP


def _version() -> str:
    try:
        return importlib.metadata.version("aiui-mcp")
    except importlib.metadata.PackageNotFoundError:
        return "dev"


VERSION = _version()
BUILD_INFO = f"aiui-mcp v{VERSION}"


# Warnings raised while parsing env knobs *before* `basicConfig` has run —
# drained through `log` the moment it exists, just below `basicConfig`.
_ENV_WARNINGS: list[str] = []


def _env_warn(message: str) -> None:
    """Queue or emit a bad-env-value warning depending on whether logging is
    configured yet. `AIUI_LOG_LEVEL` is parsed before `basicConfig`, so that
    one warning has nowhere to go until afterwards."""
    if "log" in globals():
        log.warning("%s", message)
    else:
        _ENV_WARNINGS.append(message)


def _env_float(name: str, default: float) -> float:
    """Read a float knob from the environment, never letting a typo in one
    stop the server from starting (#203).

    These parses run at import time, before FastMCP registers a single tool,
    so an unguarded `float("120s")` turns `AIUI_TIMEOUT_S=120s` into "the aiui
    MCP server failed to start", with the cause buried in a log the user may
    not know how to open — and these are exactly the knobs someone reaches
    for while debugging a flaky tunnel. A bad value falls back to the default
    and is *warned about*: silently ignoring it would leave the user believing
    a knob took effect that never did.
    """
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        return float(raw)
    except ValueError:
        _env_warn(f"{name}={raw!r} is not a number — ignoring it, using {default}")
        return default


# Spelled out rather than via `logging.getLevelNamesMapping()`, which only
# exists from 3.11 — this package supports 3.10.
_LOG_LEVELS = frozenset({"CRITICAL", "FATAL", "ERROR", "WARN", "WARNING", "INFO", "DEBUG", "NOTSET"})


def _env_log_level(default: str = "INFO") -> str:
    """Same contract as `_env_float`, for `AIUI_LOG_LEVEL`. `basicConfig`
    raises `ValueError: Unknown level: 'TRACE'` for anything that is not a
    known level name, which would kill the process just as dead."""
    raw = os.environ.get("AIUI_LOG_LEVEL")
    if raw is None:
        return default
    level = raw.strip().upper()
    if level in _LOG_LEVELS:
        return level
    _env_warn(f"AIUI_LOG_LEVEL={raw!r} is not a known log level — using {default}")
    return default


logging.basicConfig(
    level=_env_log_level(),
    format="%(asctime)s.%(msecs)03d %(levelname)s %(name)s %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
    stream=sys.stderr,
)
log = logging.getLogger("aiui")
for _queued in _ENV_WARNINGS:
    log.warning("%s", _queued)
_ENV_WARNINGS.clear()
log.info("---- %s started pid=%d ----", BUILD_INFO, os.getpid())


def _default_token_path() -> str:
    """Per-OS default location of the companion's pairing token.

    The companion (Tauri side) writes the token to its OS-correct config
    directory; this server reads from the matching path so the two sides
    agree without needing AIUI_TOKEN_PATH to be set.

    - Linux / macOS: `~/.config/aiui/token` — XDG-style. macOS keeps the
      same path as Linux so existing v0.4.x installs don't have to migrate.
    - Windows: `%APPDATA%\\aiui\\token` — matches Tauri's `dirs::config_dir()`.

    In practice the Python side runs on the Linux remote almost always —
    the Windows branch only kicks in if someone runs `aiui-mcp` directly
    on a Windows host (rare, but no longer broken).
    """
    if sys.platform == "win32":
        appdata = os.environ.get("APPDATA")
        if appdata:
            return str(Path(appdata) / "aiui" / "token")
        # Fallback if APPDATA is somehow unset — unusual on Windows.
        return str(Path.home() / "AppData" / "Roaming" / "aiui" / "token")
    return "~/.config/aiui/token"


TOKEN_PATH = Path(os.environ.get("AIUI_TOKEN_PATH", _default_token_path())).expanduser()
ENDPOINT = os.environ.get("AIUI_ENDPOINT", "http://127.0.0.1:7777")
TIMEOUT_S = _env_float("AIUI_TIMEOUT_S", 120.0)
HEALTH_TIMEOUT_S = _env_float("AIUI_HEALTH_TIMEOUT_S", 3.0)

# Cooperative version floor (Step 2). The wire contract between this bridge and
# the Mac companion is versioned independently of either side's release version.
# This bridge speaks wire v1; if the companion reports a *different*
# wire_version we surface a structured "restart this session" tool error rather
# than letting the Mac kill us to force a version. Ordinary app-version skew is
# tolerated — only an incompatible wire_version is fatal. Checked once per
# process (memoised in `_wire_checked`).
EXPECTED_WIRE_VERSION = 1
_wire_checked = False

# Cold-start poll (Step 3, bridge parity with the Rust bridge's wait_for_aiui).
# Before the first render call we poll the unauthenticated /ping until the
# companion answers or this budget elapses — so a freshly-launched Claude
# Desktop / just-up SSH tunnel gets time to start serving instead of failing
# the first call. Replaces the brittle single 3 s /health preflight.
COLDSTART_WAIT_S = _env_float("AIUI_COLDSTART_WAIT_S", 30.0)

# Per-GET timeout for the async-render poll. Must exceed the companion's
# ~25 s server-side poll window so the server always answers `{pending:true}`
# before we time out, letting us re-poll cleanly.
ASYNC_POLL_TIMEOUT_S = 40.0

# Poll-retry budget for the async render (#202). The whole point of the async
# design is that a dropped connection cannot cost the user's think-time: the
# dialog stays on the Mac's screen for the full server-side TTL, so a transport
# error on one poll is a blip, not an answer. Aborting on the first one
# abandoned an answered window and made the agent's natural retry open a
# *second* dialog for the same question. Five consecutive failures a second
# apart tolerate roughly three minutes of outage once the 40 s per-GET timeout
# is counted in — an SSH reverse-tunnel re-establish or a WebView restart during
# an in-app update fits comfortably. The counter resets on every successful
# poll, so a flaky link never accumulates its way to a false give-up. Only
# transport errors are retried: a 404 means the slot is genuinely gone.
ASYNC_POLL_MAX_CONSECUTIVE_FAILURES = 5
ASYNC_POLL_RETRY_BACKOFF_S = 1.0

# Floor between two poll iterations. A real companion holds each GET for its
# ~25 s poll window so the loop cannot spin today, but a companion that answers
# `{pending: true}` immediately would turn this into a CPU spin plus a flood of
# progress notifications.
ASYNC_POLL_MIN_INTERVAL_S = 0.2

# Fallback when a 202 body carries no `ttl_secs` — mirrors the companion's
# `DIALOG_TTL` (2 h). The advertised TTL is the wall-clock ceiling on retrying:
# past it the id is gone on the companion side.
DEFAULT_POLL_TTL_S = 7200.0

# Budget for the best-effort `DELETE /render/{id}` that retracts a dialog whose
# caller was cancelled (#193). Short: cleanup must never outlive the thing it
# cleans up.
CANCEL_RENDER_TIMEOUT_S = 2.0

# Timeout for the `upload` tool's held `POST /upload` (#146). The picker + byte
# transfer runs on one request and the user may browse their filesystem before
# choosing, so this is deliberately generous — far beyond any realistic
# file-picker think-time. A periodic progress notification keeps the MCP client
# reassured while the call is held.
UPLOAD_TIMEOUT_S = _env_float("AIUI_UPLOAD_TIMEOUT_S", 900.0)
UPLOAD_FILE_CAP = 512 * 1024 * 1024  # mirrors the companion's cap

_INSTRUCTIONS = """\
aiui is connected — you can render native dialogs on the user's Mac \
instead of asking via chat. Default behaviour for this session:

- Yes/no question (esp. before delete / drop / force-push / deploy) → \
  call `confirm` instead of asking in chat.
- Pick-one-of-N options where context per option matters → call `ask`.
- Multiple related inputs, secret, date, slider, sortable order, \
  table-row triage, image confirm/grid → call `form`.
- User wants to hand you a file from their Mac (`/aiui:upload`, \
  "take this file", "upload …") → call `upload` with the target \
  directory on your host; don't ask them to `scp` it.
- Async-completion signal the user doesn't need to answer (tests green, \
  deploy done, merge conflict) → call `notify` — it returns immediately, \
  no dialog, no reply expected.
- Pure information the user only reads → keep it in chat.

Type `/aiui:teach` for the full widget catalog when composing a \
complex form.
"""

# `instructions` is the spec-sanctioned way to push a top-level hint
# into every session at the MCP handshake — Claude Code (and Claude
# Desktop) feed it to the agent before the first turn.  Kept short on
# purpose; the full catalog lives in the `widgets`/`teach` prompts.
mcp = FastMCP("aiui", instructions=_INSTRUCTIONS)


def _token() -> str:
    if not TOKEN_PATH.exists():
        raise RuntimeError(
            f"aiui token not found at {TOKEN_PATH}. "
            "Install the aiui companion on your Mac and register this remote from its "
            "settings window (adds the token automatically). "
            "Download: https://github.com/byte5ai/aiui/releases/latest"
        )
    tok = TOKEN_PATH.read_text().strip()
    # Issue #185: a truncated or empty token must fail loudly here rather
    # than being sent as a bare `Bearer ` that the companion then rejects
    # with an opaque 401. A well-formed aiui token is 64 hex chars.
    if len(tok) != 64 or any(c not in "0123456789abcdefABCDEF" for c in tok):
        raise RuntimeError(
            f"aiui token at {TOKEN_PATH} is malformed ({len(tok)} chars, expected 64 hex). "
            "Re-register this remote from the companion's settings window on your Mac "
            "to write a fresh token."
        )
    return tok


def _explain_exc(e: BaseException) -> str:
    """Return a non-empty, human-readable description of an exception.

    httpx wraps low-level transport errors (RemoteProtocolError after
    the peer crashed mid-response, ReadError on stream close, …) where
    ``str(e)`` is empty. Without this fallback those surfaced as
    ``error: ""`` in tool responses — useless for diagnosis. The
    exception class name is always present, so it gives the user
    *something* concrete even when httpx provides no message.
    """
    msg = str(e).strip()
    return msg if msg else type(e).__name__


def _health_body(r: Any) -> dict[str, Any] | None:
    """Parse a ``/health`` response body, on any status code.

    ``None`` when the body is not a JSON object — which is itself diagnostic:
    something that isn't the companion is answering on this port. Callers must
    read the body even on a non-2xx, because since #179 it carries the
    ``reason``/``hint`` that explain the status instead of the caller having to
    guess from the number.
    """
    try:
        body = r.json()
    except Exception:  # noqa: BLE001 — any parse failure means "no usable body"
        return None
    return body if isinstance(body, dict) else None


async def _preflight() -> None:
    """Quick sanity check before every render call: the service on :7777 must
    accept our bearer token. Guards against stale local aiui instances that
    would otherwise hijack the SSH reverse-forward and hang dialogs silently.
    """
    async with httpx.AsyncClient(timeout=HEALTH_TIMEOUT_S) as client:
        try:
            r = await client.get(
                f"{ENDPOINT}/health",
                headers={"Authorization": f"Bearer {_token()}"},
            )
        except httpx.ConnectError as e:
            raise RuntimeError(
                f"aiui companion not reachable at {ENDPOINT}. "
                f"Is Claude Desktop running on your Mac? For remote projects, the "
                f"SSH reverse-tunnel must also be active (companion handles it "
                f"automatically if this host is registered in its settings). "
                f"Underlying error: {e}"
            ) from e
        except httpx.ReadTimeout as e:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} timed out on /health — likely a stale "
                f"local aiui instance holding the port. Run `pkill -f '^aiui$'` on "
                f"this host. ({_explain_exc(e)})"
            ) from e
        except httpx.ReadError as e:
            # Connected at the TCP layer but the stream closed with no HTTP
            # response — the classic remote signature of "tunnel is up but the
            # Mac side isn't serving" (stale SSH reverse-forward bound to :7777
            # with a dead aiui behind it). Distinct from ConnectError (nothing
            # listening) and from a clean 401/5xx.
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} accepted the connection but sent no "
                f"response (ReadError). On a remote this means the SSH reverse-tunnel "
                f"is up but the Mac-side aiui isn't serving — Claude Desktop may be "
                f"closed, or a stale tunnel is squatting :7777. Open Claude Desktop on "
                f"the Mac; if it persists, re-register this remote in aiui.app settings. "
                f"({_explain_exc(e)})"
            ) from e
        except httpx.RemoteProtocolError as e:
            # Connection reset / closed mid-response. The on-Mac mcp-stdio
            # child's auto-resurrect normally brings aiui.app back on the
            # next tool call, so a one-off reset is usually self-healing —
            # we name the most common stuck-state causes (stale SSH tunnel
            # squatting :7777, token mismatch from a parallel install)
            # rather than telling the user to manually restart aiui.app.
            # httpx leaves str(e) empty for this class of error — the
            # `_explain_exc` fallback surfaces the class name so the user
            # at least sees *something* concrete.
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} reset the connection. "
                f"The Mac-side mcp-stdio normally auto-resurrects aiui.app on "
                f"the next call — if this persists, a stale process may hold "
                f"the port. Verify that Claude Desktop is open on the Mac and, "
                f"on remotes, re-register the host in aiui.app settings to "
                f"re-sync the token. "
                f"({_explain_exc(e)})"
            ) from e
        except httpx.HTTPError as e:
            # Catch-all for the rest of the httpx hierarchy (RequestError,
            # WriteError, HTTPStatusError, …) so we never bubble up a bare
            # exception with an empty message.
            raise RuntimeError(
                f"aiui companion request to {ENDPOINT} failed: {_explain_exc(e)}. "
                f"Verify Claude Desktop is open on the Mac; auto-resurrect "
                f"normally restores the GUI on the next call. If repeated, "
                f"check the SSH reverse-tunnel and re-register this remote "
                f"in aiui.app settings to re-sync the token."
            ) from e

        if r.status_code == 401:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} rejected our token (401). "
                f"Another aiui process may be listening on this port with a different "
                f"token. Run `pkill -f '^aiui$'` on this host, then re-register it "
                f"from the companion's settings window to re-sync the token."
            )
        body = _health_body(r)

        if r.status_code != 200:
            # Only a genuinely unserviceable companion reaches here: since
            # #179 the companion answers 503 for `webview_unresponsive` alone.
            # Relay its `hint` verbatim — the old `r.text[:200]` handed the
            # user a status code and half a JSON blob.
            hint = (body.get("hint") or body.get("reason")) if body else None
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} is not serving "
                f"(HTTP {r.status_code}): {hint or r.text[:200]}"
            )

        if body is None:
            raise RuntimeError(
                f"aiui companion /health returned a 200 with an unparseable body: "
                f"{r.text[:200]}. Another process may be holding {ENDPOINT}."
            )

        # Degraded-but-serving (`dialog_registry_full`, `too_many_children`)
        # comes back as 200 + `ready: false`. It must NOT block this session:
        # /render sweeps expired dialogs and evicts the oldest on its own, so
        # one session's backlog used to take rendering down for every other
        # session sharing the companion (#179).
        if body.get("ready") is False:
            log.warning(
                "aiui companion degraded but serving (reason=%s): %s",
                body.get("reason") or "unknown",
                body.get("hint") or "",
            )

        # Cooperative version floor (Step 2): once per process, confirm the
        # companion speaks a compatible wire version. Reuses this client.
        await _check_wire_compat(client)


async def _check_wire_compat(client: httpx.AsyncClient) -> None:
    """One-time wire-compatibility check against the companion's `/version`.

    Raises a structured ``RuntimeError`` (surfaced to the agent as a tool error)
    on a hard wire-version mismatch, telling the user to restart this Claude
    Code session so it respawns ``aiui-mcp`` at a matching version — the
    cooperative replacement for the Mac externally killing this bridge.

    Tolerant by design: a companion too old to report ``wire_version`` (field
    absent → treated as v1), or any transient error reading ``/version``, does
    NOT block — we only hard-fail on an explicit, incompatible ``wire_version``.
    Memoised via the module-level ``_wire_checked`` so it costs one extra GET
    per process, on the first render only.
    """
    global _wire_checked
    if _wire_checked:
        return
    try:
        r = await client.get(
            f"{ENDPOINT}/version",
            headers={"Authorization": f"Bearer {_token()}"},
        )
        r.raise_for_status()
        remote_wire = int(r.json().get("wire_version", EXPECTED_WIRE_VERSION))
    except Exception as e:  # noqa: BLE001 — tolerate skew; never block on a read error
        log.debug("wire-compat check skipped (could not read /version): %s", _explain_exc(e))
        _wire_checked = True
        return
    if remote_wire != EXPECTED_WIRE_VERSION:
        # Do NOT memoise a failure — leave it un-set so a subsequent call
        # (e.g. after the user restarts the companion) re-checks cleanly.
        raise RuntimeError(
            f"incompatible aiui versions — this bridge (aiui-mcp {VERSION}) speaks wire "
            f"v{EXPECTED_WIRE_VERSION}, but the companion on your Mac speaks wire "
            f"v{remote_wire}. Restart this Claude Code session so it respawns aiui-mcp at "
            f"a matching version (or update the side that is behind)."
        )
    _wire_checked = True


_SRC_KEYS = {"src", "thumbnail"}
_MAX_IMAGE_BYTES = 10 * 1024 * 1024  # 10 MB — mirrors the Rust resolver
# Largest single clip the companion's `/media` cache accepts (#194). Mirrors
# `media::MEDIA_FILE_CAP` in the Rust half; checked here *before* the read so
# an oversize file is skipped rather than pulled into RAM first.
_MEDIA_FILE_CAP = 512 * 1024 * 1024
_LOCAL_PATH_MIME_OVERRIDES = {
    # mimetypes.guess_type returns None for SVG without a hint on some
    # Pythons, and `image/svg` (without `+xml`) on others. Lock it down
    # so the WebView always sees the canonical `image/svg+xml`.
    ".svg": "image/svg+xml",
    # Audio (#25): `mimetypes.guess_type` returns None or an inconsistent
    # value for several of these across Python/OS combinations (notably
    # `.m4a` and `.flac`). Local audio is normally routed through
    # `_upload_local_audios` before this function ever runs, but pin these
    # down anyway so the `data:` fallback path stays correct too — mirrors
    # `guess_mime_from_extension` in the Rust bridge.
    ".mp3": "audio/mpeg",
    ".m4a": "audio/mp4",
    ".wav": "audio/wav",
    ".aac": "audio/aac",
    ".ogg": "audio/ogg",
    ".flac": "audio/flac",
}


# Module-level rather than computed per call so a test can monkeypatch it
# and exercise the Windows branch on a POSIX runner — the same reason the
# Rust mirror takes the platform as a parameter (#201).
_IS_WINDOWS = sys.platform == "win32"


def _is_windows_abs_path(s: str) -> bool:
    """`C:\\foo`, `D:/bar`, long-path `\\\\?\\C:\\…`, UNC `\\\\server\\share\\…`.

    Mirrors the `cfg!(windows)` branch of `imageresolve::looks_like_local_path`.
    """
    if s.startswith("\\\\"):
        return True
    return (
        len(s) >= 3
        and s[0].isascii()
        and s[0].isalpha()
        and s[1] == ":"
        and s[2] in "\\/"
    )


def _looks_like_local_path(s: str) -> bool:
    """Mirror of `imageresolve::looks_like_local_path` in the Rust bridge.

    Accepts absolute paths and `~`-rooted paths, plus — on a Windows host
    only — drive-letter, long-path and UNC paths. Rejects `data:` URLs,
    `http(s)://` URLs, relative paths (no stable cwd contract on MCP
    bridges), and anything else.

    The Windows shapes are gated on the host platform on purpose: `C:\\x.png`
    is not a path on Linux, and accepting it there would push garbage into
    `_read_path_as_data_url` instead of leaving the value alone.
    """
    if not s:
        return False
    if s.startswith(("data:", "http://", "https://")):
        return False
    if s.startswith("/") or s.startswith("~"):
        return True
    return _IS_WINDOWS and _is_windows_abs_path(s)


def _read_path_as_data_url(raw: str) -> str:
    """Read a local file and return it as `data:<mime>;base64,…`.

    Raises ValueError on anything that should make the resolver leave
    the original `src` value alone (missing file, oversize, not a file,
    an unexpandable `~`).
    """
    try:
        path = Path(raw).expanduser()
    except RuntimeError as e:
        # `expanduser()` raises RuntimeError — not OSError — when it cannot
        # determine a home directory, which is what `~someuser/x.png` and
        # `~\Pictures\x.png` do on POSIX. Left unmapped it escaped
        # `_resolve_local_paths`'s handler and failed the whole tool call,
        # breaking this module's fail-soft contract (#201).
        raise ValueError(f"cannot expand {raw}: {e}") from e
    if not path.is_file():
        raise ValueError(f"not a file: {path}")
    size = path.stat().st_size
    if size > _MAX_IMAGE_BYTES:
        raise ValueError(f"too large: {size} bytes (max {_MAX_IMAGE_BYTES})")
    ext = path.suffix.lower()
    mime = _LOCAL_PATH_MIME_OVERRIDES.get(ext)
    if mime is None:
        mime, _ = mimetypes.guess_type(str(path))
    if mime is None:
        mime = "application/octet-stream"
    data = path.read_bytes()
    b64 = base64.b64encode(data).decode("ascii")
    return f"data:{mime};base64,{b64}"


def _resolve_local_paths(node: Any) -> None:
    """Walk a render spec in place, replacing absolute / `~/` paths in
    `src` / `thumbnail` properties with `data:` URLs. The bridge-side
    counterpart to the Mac's HTTPS resolver — runs wherever this MCP
    server runs (local or remote), which is by definition the host
    that holds the agent's files.

    Fail-soft: read errors are logged, the original value is kept (the
    WebView will eventually show a broken image rather than the call
    blowing up).
    """
    if isinstance(node, dict):
        for key, value in list(node.items()):
            if key in _SRC_KEYS and isinstance(value, str) and _looks_like_local_path(value):
                try:
                    node[key] = _read_path_as_data_url(value)
                except (OSError, ValueError) as e:
                    log.warning("local path skipped for %s: %s", value, e)
            else:
                _resolve_local_paths(value)
    elif isinstance(node, list):
        for item in node:
            _resolve_local_paths(item)


_VIDEO_EXTS = (".mp4", ".mov", ".m4v", ".webm")


def _is_local_video(s: str) -> bool:
    """A local-filesystem path pointing at a video by extension. Mirrors
    `is_local_video_path` in the Rust bridge and `isVideo` in Gallery.svelte.
    """
    if not _looks_like_local_path(s):
        return False
    stem = s.lower().split("?", 1)[0].split("#", 1)[0]
    return stem.endswith(_VIDEO_EXTS)


def _collect_local_videos(node: Any, out: list[str]) -> None:
    """Gather distinct local video paths from every `src`/`thumbnail` slot."""
    if isinstance(node, dict):
        for key, value in node.items():
            if key in _SRC_KEYS and isinstance(value, str) and _is_local_video(value):
                if value not in out:
                    out.append(value)
            else:
                _collect_local_videos(value, out)
    elif isinstance(node, list):
        for item in node:
            _collect_local_videos(item, out)


def _replace_srcs(node: Any, mapping: dict[str, str]) -> None:
    """Swap `src`/`thumbnail` strings that are keys in `mapping`."""
    if isinstance(node, dict):
        for key, value in list(node.items()):
            if key in _SRC_KEYS and isinstance(value, str) and value in mapping:
                node[key] = mapping[value]
            else:
                _replace_srcs(value, mapping)
    elif isinstance(node, list):
        for item in node:
            _replace_srcs(item, mapping)


async def _read_media_file(p: str) -> bytes:
    """Read a local media file for the `/media` push, size-checked first (#194).

    Two things the old inline `read_bytes()` got wrong. It learned the size
    only *after* materialising the file, so a 3–4 GB screen recording — an
    ordinary thing to hand a `gallery` — was pulled into RAM before anything
    compared it to the companion's 512 MB ceiling; the `MemoryError` that can
    follow is not an `OSError`, so it escaped the best-effort handler and
    killed the whole tool call instead of skipping one clip. And it was
    blocking I/O inside an `async def`, stalling the event loop — and with it
    the progress heartbeat — for the whole read.

    Raises `ValueError` for a file that is missing, not a file, or over the
    cap; `OSError` for a read that fails. Same shape as
    `_read_path_as_data_url`.
    """
    path = Path(p).expanduser()
    if not path.is_file():
        raise ValueError(f"not a file: {path}")
    size = path.stat().st_size
    if size > _MEDIA_FILE_CAP:
        raise ValueError(f"too large: {size} bytes (max {_MEDIA_FILE_CAP})")
    return await asyncio.to_thread(path.read_bytes)


async def _upload_local_videos(spec: dict[str, Any], client: httpx.AsyncClient) -> list[str]:
    """Push local video files to the companion's `/media` cache and rewrite
    their `src`/`thumbnail` to the returned loopback playback URL.

    Videos are too big to inline as `data:` (the 10 MB cap + base64 bloat),
    and a remote agent's file isn't readable from the Mac — so the bridge
    streams the bytes over the same :7777 channel the render uses (loopback
    locally, reverse tunnel remotely). Best-effort: a read error, a 413, or
    an old companion without `/media` (404) leaves the path untouched, and
    `_resolve_local_paths` then does whatever it can with it.

    Returns one human-readable warning per clip that did not make it (#194).
    The render still happens — only the silence was wrong: the user saw a
    broken player while the agent believed the clip was on screen.
    """
    paths: list[str] = []
    _collect_local_videos(spec, paths)
    warnings: list[str] = []
    if not paths:
        return warnings
    mapping: dict[str, str] = {}
    for p in paths:
        try:
            data = await _read_media_file(p)
        except (OSError, ValueError, MemoryError, RuntimeError) as e:
            # RuntimeError: `expanduser()` on an unexpandable `~user` /
            # `~\…` path — fail soft here too (#201).
            log.warning("video skipped (read failed) %s: %s", p, e)
            warnings.append(f"video not shown — {p}: {e}")
            continue
        ext = p.lower().split("?", 1)[0].split("#", 1)[0].rsplit(".", 1)[-1] or "mp4"
        try:
            r = await client.post(
                f"{ENDPOINT}/media",
                params={"ext": ext},
                headers={
                    "Authorization": f"Bearer {_token()}",
                    "Content-Type": "application/octet-stream",
                },
                content=data,
            )
            r.raise_for_status()
            url = r.json().get("url")
        except (httpx.HTTPError, ValueError) as e:
            log.warning("video upload failed %s: %s", p, e)
            warnings.append(f"video not shown — {p}: {e}")
            continue
        if url:
            mapping[p] = url
    _replace_srcs(spec, mapping)
    return warnings


_AUDIO_EXTS = (".mp3", ".m4a", ".wav", ".aac", ".ogg", ".flac")


def _is_local_audio(s: str) -> bool:
    """A local-filesystem path pointing at an audio file by extension.
    Mirrors `is_local_audio_path` in the Rust bridge (#25). Covers the
    common lossy/lossless formats a TTS sample, voice memo, or generated
    sound clip is likely to arrive in.
    """
    if not _looks_like_local_path(s):
        return False
    stem = s.lower().split("?", 1)[0].split("#", 1)[0]
    return stem.endswith(_AUDIO_EXTS)


def _collect_local_audios(node: Any, out: list[str]) -> None:
    """Gather distinct local audio paths from every `src`/`thumbnail` slot."""
    if isinstance(node, dict):
        for key, value in node.items():
            if key in _SRC_KEYS and isinstance(value, str) and _is_local_audio(value):
                if value not in out:
                    out.append(value)
            else:
                _collect_local_audios(value, out)
    elif isinstance(node, list):
        for item in node:
            _collect_local_audios(item, out)


async def _upload_local_audios(spec: dict[str, Any], client: httpx.AsyncClient) -> list[str]:
    """Push local audio files to the companion's `/media` cache and rewrite
    their `src`/`thumbnail` to the returned loopback playback URL (#25).

    Same reasoning as `_upload_local_videos`: local files are too big (or
    simply unnecessary) to inline as `data:` given the 10 MB cap + base64
    bloat, and a remote agent's file isn't readable from the Mac — so the
    bridge streams the bytes over the same :7777 channel the render uses.
    Best-effort: a read error, a 413, or an old companion without `/media`
    (404) leaves the path untouched, and `_resolve_local_paths` then does
    whatever it can with it.

    Returns one warning per clip that did not make it (#194), same contract
    as the video half.
    """
    paths: list[str] = []
    _collect_local_audios(spec, paths)
    warnings: list[str] = []
    if not paths:
        return warnings
    mapping: dict[str, str] = {}
    for p in paths:
        try:
            data = await _read_media_file(p)
        except (OSError, ValueError, MemoryError, RuntimeError) as e:
            # RuntimeError: `expanduser()` on an unexpandable `~user` /
            # `~\…` path — fail soft here too (#201).
            log.warning("audio skipped (read failed) %s: %s", p, e)
            warnings.append(f"audio not played — {p}: {e}")
            continue
        ext = p.lower().split("?", 1)[0].split("#", 1)[0].rsplit(".", 1)[-1] or "mp3"
        try:
            r = await client.post(
                f"{ENDPOINT}/media",
                params={"ext": ext},
                headers={
                    "Authorization": f"Bearer {_token()}",
                    "Content-Type": "application/octet-stream",
                },
                content=data,
            )
            r.raise_for_status()
            url = r.json().get("url")
        except (httpx.HTTPError, ValueError) as e:
            log.warning("audio upload failed %s: %s", p, e)
            warnings.append(f"audio not played — {p}: {e}")
            continue
        if url:
            mapping[p] = url
    _replace_srcs(spec, mapping)
    return warnings


def _collect_target_fields(spec: dict[str, Any]) -> list[dict[str, Any]]:
    """Form fields carrying a non-null `target` (#135), from flat `fields` and
    any `tabs[].fields`."""
    out: list[dict[str, Any]] = []

    def scan(fields: Any) -> None:
        if isinstance(fields, list):
            for f in fields:
                # #199: `isinstance(..., dict)`, not `is not None` — a
                # `"target": "~/x"` string used to be collected here and then
                # `.get()`-ed in `_write_local_target`, killing the tool call
                # with `AttributeError` *after* the user had typed a secret.
                # A current companion rejects that shape at validate_spec; a
                # non-dict target that still reaches us is simply not a
                # target (a `secret` carrying one is still stripped by
                # `_collect_secret_fields`).
                if isinstance(f, dict) and isinstance(f.get("target"), dict) and isinstance(f.get("name"), str):
                    out.append(f)

    scan(spec.get("fields"))
    for tab in spec.get("tabs") or []:
        if isinstance(tab, dict):
            scan(tab.get("fields"))
    return out


def _collect_secret_fields(spec: dict[str, Any]) -> list[str]:
    """Names of every `secret`-kind field, with or without a `target` (#186).

    The write-only contract is a property of the KIND — the docs say a
    secret's value is never returned — but the strip used to key on `target`,
    so a target-less `secret` travelled back to the agent as plaintext.
    """
    out: list[str] = []

    def scan(fields: Any) -> None:
        if isinstance(fields, list):
            for f in fields:
                if (
                    isinstance(f, dict)
                    and f.get("kind") == "secret"
                    and isinstance(f.get("name"), str)
                ):
                    out.append(f["name"])

    scan(spec.get("fields"))
    for tab in spec.get("tabs") or []:
        if isinstance(tab, dict):
            scan(tab.get("fields"))
    return out


def _target_path_error(raw_path: str) -> str | None:
    """Why a `target.path` is unusable, or None. Mirror of the Rust
    `filewrite::target_path_error` — same rule, same wording, because #199 was
    exactly the two bridges quietly accepting different paths.

    A relative path has no stable cwd to resolve against (the bridge's is the
    agent's, the Finder-launched companion's is typically `/`) and `~user/`
    only ever worked on this side, so both are rejected rather than written
    somewhere the user never approved. Same rule `_upload_expand_dir`
    enforces for `upload`'s `target_dir`.
    """
    if not raw_path or len(raw_path) > 4096 or any(
        ord(c) < 0x20 or ord(c) == 0x7f for c in raw_path
    ):
        return "invalid target path"
    # `startswith("/")` in addition to `is_absolute()` so a POSIX-style path
    # is accepted identically on a Windows bridge host, matching the Rust
    # side's `has_root()`.
    if raw_path.startswith("~/") or raw_path.startswith("/") or Path(raw_path).is_absolute():
        return None
    return f"target path must be an absolute or ~/-rooted path, got '{raw_path}'"


def _resolve_target_path(path: Path) -> Path:
    """Resolve the destination a write really lands on (#199). Mirror of the
    Rust `filewrite::resolve_target`.

    `Path.resolve(strict=True)` on the whole path is wrong here: for `create`
    the file does not exist yet. So canonicalise the *parent* and re-join the
    name, then follow the final component while it is an existing symlink —
    otherwise `substitute` reads through the link but `os.replace`s a fresh
    regular file over the link itself, leaving the real config untouched and
    the secret in what used to be the link.
    """
    cur = path
    for _ in range(32):  # bounded, so a symlink cycle can't spin here
        try:
            base = cur.parent.resolve(strict=True) / cur.name
        except OSError:
            base = cur  # parent doesn't exist yet — nothing to resolve
        if not base.is_symlink():
            return base
        try:
            dest = Path(os.readlink(base))
        except OSError:
            return base
        cur = dest if dest.is_absolute() else base.parent / dest
    return cur


def _write_local_target(value: str, target: Any) -> dict[str, Any]:
    """Mirror of the Rust `filewrite::write_local`: a LOCAL file write on THIS
    host (the bridge runs where the agent runs, so the file is always local).
    `create` (atomic tmp+rename, refuses clobber without overwrite) or
    `substitute` (replace a placeholder occurring exactly once). Never logs the
    value. Returns `{written, target, bytes, mode?, error?}`.

    #199: nothing here may escape as an exception. Every failure happens
    *after* the user has typed a value they cannot retype (a credential), so
    a traceback loses the secret and tells the agent nothing. Structured
    outcome or nothing.
    """
    if not isinstance(target, dict):
        # The Python equivalent of Rust's `WriteOutcome::invalid` — no
        # destination to even name.
        return {"written": False, "target": "", "bytes": 0,
                "error": "target must be an object with mode/path"}
    raw_path = str(target.get("path", ""))
    why = _target_path_error(raw_path)
    if why:
        return {"written": False, "target": raw_path, "bytes": 0, "error": why}
    try:
        path = _resolve_target_path(Path(raw_path).expanduser())
    except Exception:  # pragma: no cover - expanduser on an exotic home
        path = Path(raw_path)
    display = str(path)
    # Issue #177: an empty credential is never a legitimate write, and
    # truncating the user's file is not a dialog's job. Refusing here, before
    # the mode dispatch, covers `create` (which would clobber under
    # `overwrite`) *and* `substitute` (which needs no `overwrite` and would
    # erase the sentinel, making a retry impossible). Mirrors the identical
    # guard in Rust `filewrite::write_local`.
    if value == "":
        return {"written": False, "target": display, "bytes": 0,
                "error": "refusing to write an empty value"}
    mode = target.get("mode")
    perm_s = target.get("perm")
    try:
        perm: int | None = int(str(perm_s), 8) if perm_s else None
    except ValueError:
        perm = None
    if perm is None:
        # #199: one default per mode, matching Rust. `create` stays tight by
        # default — a fresh credential file must never inherit the umask.
        # `substitute` is editing a file the user already owns, and silently
        # re-chmod'ing a 0644 compose file to 0600 stopped the container that
        # read it; it keeps the destination's mode instead.
        if mode == "substitute":
            try:
                perm = path.stat().st_mode & 0o7777
            except OSError:
                perm = 0o600  # unreadable → the read below fails anyway
        else:
            perm = 0o600
    # Mirrors Rust's `#[cfg(unix)]` guard: off POSIX there are no mode bits
    # (and `os.fchmod` does not exist — an unguarded call made *every* target
    # write on a Windows bridge host die with AttributeError).
    can_chmod = hasattr(os, "fchmod")
    mode_out: dict[str, Any] = {"mode": f"{perm:04o}"} if can_chmod else {}

    def atomic_write(p: Path, data: bytes) -> None:
        p.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(prefix=".aiui-write-", dir=str(p.parent))
        try:
            if can_chmod:
                os.fchmod(fd, perm)
            with os.fdopen(fd, "wb") as f:
                f.write(data)
            os.replace(tmp, p)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise

    try:
        if mode == "create":
            if path.exists() and not target.get("overwrite"):
                return {"written": False, "target": display, "bytes": 0,
                        "error": "file exists and overwrite is false (mode: create)"}
            data = value.encode("utf-8")
            atomic_write(path, data)
            return {"written": True, "target": display, "bytes": len(data), **mode_out}
        if mode == "substitute":
            placeholder = target.get("placeholder")
            if not placeholder:
                return {"written": False, "target": display, "bytes": 0,
                        "error": "substitute mode requires 'placeholder'"}
            # #199: explicit UTF-8 on BOTH sides. The read used to take the
            # process locale while the write-back was always UTF-8, so on a
            # latin-1 host every non-ASCII byte in the user's file was
            # silently re-encoded by a one-line substitution. A non-UTF-8
            # target now fails structurally (UnicodeDecodeError is caught
            # below), exactly as the Rust `read_to_string` already did —
            # `errors="replace"` would trade a loud failure for silent
            # corruption, which is the bug, not the fix.
            existing = path.read_text(encoding="utf-8")
            count = existing.count(placeholder)
            if count != 1:
                return {"written": False, "target": display, "bytes": 0,
                        "error": (f"placeholder '{placeholder}' not found in target file"
                                  if count == 0
                                  else f"placeholder '{placeholder}' found {count}× (must be exactly 1)")}
            updated = existing.replace(placeholder, value, 1)
            data = updated.encode("utf-8")
            atomic_write(path, data)
            return {"written": True, "target": display, "bytes": len(data), **mode_out}
        return {"written": False, "target": display, "bytes": 0, "error": f"unknown mode '{mode}'"}
    except (OSError, ValueError) as e:
        # ValueError covers UnicodeDecodeError (a non-UTF-8 target), which is
        # NOT an OSError and used to escape as a raw traceback.
        return {"written": False, "target": display, "bytes": 0, "error": str(e)}
    except Exception as e:  # pragma: no cover - backstop, see the docstring
        return {"written": False, "target": display, "bytes": 0,
                "error": f"target write failed: {e.__class__.__name__}: {e}"}


def _action_commits_targets(spec: dict[str, Any], action: Any) -> bool:
    """Issue #177: does the action the user pressed commit `target` file
    writes? Mirror of the Rust `action_commits_targets`, resolved from the
    spec the agent submitted.

    The built-in submit (`action: None`/absent) always commits. A named action
    commits unless it carries `skip_validation: True` — documented as an escape
    hatch so required-field validation never traps the user, and an escape
    hatch is by definition non-committing. `writes_targets: True` is the
    explicit opt-in for an action that needs both. An action not found in the
    spec fails closed.

    Deliberately not keyed on `destructive`/`primary`: both are styling and
    orthogonal to committing.
    """
    if action is None:
        return True
    actions = spec.get("actions")
    if not isinstance(actions, list):
        return False  # named action but no action list — fail closed
    entry = next(
        (a for a in actions if isinstance(a, dict) and a.get("value") == action),
        None,
    )
    if entry is None:
        return False  # unknown action — fail closed
    if entry.get("writes_targets") is True:
        return True
    return entry.get("skip_validation") is not True


def _annotate_target_paths(spec: dict[str, Any]) -> None:
    """Stamp every `target`-carrying field with `target.resolved_path` — the
    absolute destination THIS host will write — so the dialog's approval line
    names the file rather than the raw spec string (#199). The companion
    renders the spec but the write happens here, so this is the only place
    that can resolve it truthfully. Mutates `spec` in place; always
    overwrites, so an agent-supplied value can't misstate the destination.
    """

    def scan(fields: Any) -> None:
        if not isinstance(fields, list):
            return
        for f in fields:
            if not isinstance(f, dict) or not isinstance(f.get("target"), dict):
                continue
            raw = f["target"].get("path")
            if not isinstance(raw, str) or _target_path_error(raw):
                continue
            try:
                f["target"]["resolved_path"] = str(
                    _resolve_target_path(Path(raw).expanduser())
                )
            except Exception:  # pragma: no cover - display only, never fatal
                pass

    scan(spec.get("fields"))
    for tab in spec.get("tabs") or []:
        if isinstance(tab, dict):
            scan(tab.get("fields"))


def _apply_target_writes(spec: dict[str, Any], data: dict[str, Any]) -> None:
    """After a render returns, perform the local file writes for `target`
    fields on THIS host and fold the outcomes back into the result, stripping
    raw `secret` values so they never reach the agent. No-op on cancel or when
    no field carries a target. Mutates `data` in place.

    Issue #177: only an affirmative action commits the writes. A
    non-committing action (and a field absent from the payload) still yields a
    per-field outcome, so the agent gets a reason rather than silence — and a
    `secret` value is stripped either way.
    """
    if data.get("cancelled"):
        return
    targets = _collect_target_fields(spec)
    secrets = _collect_secret_fields(spec)
    # #186: a target-less `secret` is exactly the case the old
    # `if not targets: return` short-circuited past, leaking the plaintext.
    if not targets and not secrets:
        return
    result = data.setdefault("result", {})
    values = result.setdefault("values", {})
    action = result.get("action")
    commits = _action_commits_targets(spec, action)
    for field in targets:
        name = field["name"]
        v = values.get(name)
        if not commits:
            label = action if action is not None else "(submit)"
            outcome = {"written": False, "target": str(field["target"].get("path", "")),
                       "bytes": 0,
                       "error": f"action '{label}' does not commit target writes"}
        elif name not in values or v is None:
            # "Absent from the payload" is not "submitted blank": never
            # launder a missing key into an empty write.
            outcome = {"written": False, "target": str(field["target"].get("path", "")),
                       "bytes": 0, "error": "no value submitted for this field"}
        else:
            try:
                outcome = _write_local_target(str(v), field["target"])
            except Exception as e:  # pragma: no cover - belt and braces
                # #199: one bad field used to abort the loop mid-way, so the
                # writes that had already landed were never reported and the
                # tool call died with a traceback. Every field gets an
                # outcome; nothing propagates out of the submit path.
                outcome = {"written": False,
                           "target": str(field["target"].get("path", "")),
                           "bytes": 0,
                           "error": f"target write failed: {e.__class__.__name__}: {e}"}
        if field.get("kind") == "secret":
            values[name] = outcome  # write-only: raw value never returned
        else:
            values[name] = {"value": v, **outcome}

    # #186: a `secret` carrying no `target` was never in `targets`, so nothing
    # above replaced it. A current companion rejects that shape at
    # validate_spec, but an older companion in front of this bridge would not
    # — and the value must not reach the agent regardless of which side is
    # newer.
    for name in secrets:
        v = values.get(name)
        if isinstance(v, dict) and "written" in v:
            continue  # already replaced by a write outcome above
        if name in values:
            values[name] = {
                "written": False,
                "target": "",
                "bytes": 0,
                "error": "secret field has no target — value discarded, never returned",
            }


def _upload_safe_base_name(raw: str) -> str | None:
    """Reduce a filename to a safe base name (#146): strip any directory
    components so a selection can never escape the target dir, reject
    empty / `.` / `..`. Mirrors `safe_base_name` in the Rust bridge.
    """
    base = os.path.basename(raw).strip()
    if not base or base in (".", ".."):
        return None
    return base


def _upload_expand_dir(raw: str) -> Path | None:
    """Expand a `~/`-rooted or absolute target directory. Returns None for a
    relative path — there is no stable cwd contract to resolve it against, so
    it's treated as a caller error (the cwd default is applied by the caller
    only when `target_dir` is absent). Mirrors `expand_dir` in the Rust bridge.

    #194: only `~` and `~/…` are expandable, byte-for-byte the Rust rule. Any
    other leading `~` — `~nosuchuser/x` — used to reach `expanduser()`, which
    raises `RuntimeError: Could not determine home directory` for an unknown
    user. That escaped the `upload` tool entirely and broke its contract that
    every failure comes back as `{status: "error", error}`. It is now just
    another unexpandable path, and the caller says so in words.
    """
    if raw == "~" or raw.startswith("~/"):
        try:
            return Path(raw).expanduser()
        except (RuntimeError, OSError):
            return None
    if raw.startswith("~"):
        return None
    p = Path(raw)
    return p if p.is_absolute() else None


def _upload_write(dest_dir: Path, filename: str, data: bytes) -> dict[str, Any]:
    """Atomically write the uploaded bytes to `dest_dir/<filename>` on THIS host,
    never overwriting an existing file. Mirrors `do_upload`'s write half in the
    Rust bridge. Returns the `{status, …}` payload.
    """
    dest = dest_dir / filename
    try:
        fd, tmp = tempfile.mkstemp(prefix=".aiui-upload-", dir=str(dest_dir))
        try:
            with os.fdopen(fd, "wb") as f:
                f.write(data)
                f.flush()
                os.fsync(f.fileno())
            # Hard-link into place: atomic, and raises FileExistsError if the
            # destination already exists — no check-then-write race window a
            # prior `exists()` guard + `os.replace` left open (codex review P2).
            try:
                os.link(tmp, dest)
            except FileExistsError:
                return {"status": "error", "error": f"target already exists, not overwriting: {dest}"}
            except OSError as e:
                # #194: exFAT/FAT32 and many SMB mounts have no hard links, so
                # an upload to a USB stick or a share failed *after* the bytes
                # had crossed the tunnel, with an OS message that named no
                # cause the user could act on. Fall back to creating the
                # destination with O_EXCL: no-clobber survives (that is the
                # promise), atomicity does not (which nothing promises).
                log.info("hard link unsupported for %s (%s) — writing directly", dest, e)
                _upload_write_exclusive(dest, data)
        finally:
            try:
                os.unlink(tmp)
            except OSError:
                pass
    except FileExistsError:
        return {"status": "error", "error": f"target already exists, not overwriting: {dest}"}
    except OSError as e:
        return {"status": "error", "error": f"writing {dest}: {e}"}
    return {"status": "ok", "path": str(dest), "filename": filename, "bytes": len(data)}


def _upload_write_exclusive(dest: Path, data: bytes) -> None:
    """Create `dest` with `O_EXCL` and write `data` into it (#194).

    The never-clobber half of `_upload_write` for filesystems without hard
    links. Raises `FileExistsError` when the destination is taken — the same
    signal `os.link` gives — and cleans up a partially written file it created
    itself, so a retry doesn't trip over our own debris. Mirrors
    `fsutil::write_new_unlinked` in the Rust bridge.
    """
    fd = os.open(dest, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
    except BaseException:
        try:
            os.unlink(dest)
        except OSError:
            pass
        raise


async def _upload_heartbeat(ctx: Context) -> None:
    """Emit a progress notification every ~10 s while `POST /upload` is held, so
    the MCP client knows the tool is alive while the user browses the picker.
    Cancelled by the caller once the POST returns. Best-effort — a missing
    progressToken or any reporting hiccup must never break the upload.
    """
    iteration = 0
    try:
        while True:
            await asyncio.sleep(10)
            iteration += 1
            try:
                await ctx.report_progress(progress=float(iteration), total=None)
            except Exception as e:  # noqa: BLE001
                log.debug("upload progress skipped: %s", _explain_exc(e))
    except asyncio.CancelledError:
        pass


async def _wait_for_aiui() -> None:
    """Poll the unauthenticated `/ping` until the companion answers or
    `COLDSTART_WAIT_S` elapses (Step 3, parity with the Rust bridge).

    Gives a cold companion (Claude Desktop just launched, SSH tunnel just came
    up) time to start serving before the first render, instead of failing the
    call outright. Tolerant: on timeout we simply fall through to `_preflight`,
    which produces the precise reachability diagnosis. `/ping` is cheap and
    needs no token, so this is a light readiness gate, not a full health check.
    """
    deadline = time.monotonic() + COLDSTART_WAIT_S
    async with httpx.AsyncClient(timeout=2.0) as client:
        while time.monotonic() < deadline:
            try:
                r = await client.get(f"{ENDPOINT}/ping")
                if r.status_code == 200:
                    return
            except httpx.HTTPError:
                pass  # not up yet — keep polling within the budget
            await asyncio.sleep(0.5)


async def _cancel_render(render_id: str) -> None:
    """Best-effort `DELETE /render/{id}` (#193) — tell the companion to retract a
    dialog this bridge no longer has a caller for, so it doesn't sit on the
    user's Mac waiting for an agent that is gone.

    Builds its own client on purpose: the `async with httpx.AsyncClient(...)` in
    `_post_render` may already be unwinding when this runs. Every failure is
    swallowed, and a `404`/`405` simply means an older companion without the
    route — nothing to do, never an error.
    """
    try:
        async with httpx.AsyncClient(timeout=CANCEL_RENDER_TIMEOUT_S) as client:
            r = await client.delete(
                f"{ENDPOINT}/render/{render_id}",
                headers={"Authorization": f"Bearer {_token()}"},
            )
            if r.status_code in (404, 405):
                log.debug("cancel render %s: companion has no DELETE route", render_id)
            else:
                log.info("cancelled render %s (http %s)", render_id, r.status_code)
    except Exception as e:  # noqa: BLE001
        log.debug("cancel render %s failed: %s", render_id, _explain_exc(e))


async def _poll_render(
    client: httpx.AsyncClient,
    render_id: str,
    ctx: Context | None,
    ttl_secs: float = DEFAULT_POLL_TTL_S,
) -> dict[str, Any]:
    """Poll `GET /render/{id}` until the terminal result (Step 3 async render).

    Each GET is bounded by `ASYNC_POLL_TIMEOUT_S` (> the server's ~25 s poll
    window), so the server always answers `{pending:true}` before we time out
    and we re-poll — no single connection is held for the user's think-time,
    which is what immunises the remote path against the multi-minute-ReadError
    class. Emits an MCP progress notification each pending iteration so the
    client (Claude Code) knows the tool is alive, not hung.

    On cancellation (#193) the dialog is retracted before the exception
    propagates. The MCP SDK already cancels this task on a `CancelledNotification`,
    so the task died today but the window on the Mac did not.

    #202: a transport error on one poll is retried against the SAME id rather
    than ending the call. The dialog is already on the user's screen and stays
    there for `ttl_secs`; aborting here abandons it and makes the agent's retry
    open a second window for the same question. Never re-POST `/render` — the
    id from the 202 is the whole point. Bounded by
    `ASYNC_POLL_MAX_CONSECUTIVE_FAILURES` and by the advertised TTL.
    """
    poll_url = f"{ENDPOINT}/render/{render_id}"
    iteration = 0
    consecutive_failures = 0
    deadline = time.monotonic() + ttl_secs
    try:
        while True:
            try:
                pr = await client.get(
                    poll_url,
                    headers={"Authorization": f"Bearer {_token()}"},
                    timeout=ASYNC_POLL_TIMEOUT_S,
                )
            except httpx.HTTPError as e:
                consecutive_failures += 1
                if (
                    consecutive_failures >= ASYNC_POLL_MAX_CONSECUTIVE_FAILURES
                    or time.monotonic() >= deadline
                ):
                    # `_explain_exc` guarantees a non-empty message: httpx leaves
                    # `str(e)` empty for RemoteProtocolError / ReadError, which is
                    # exactly the class that shows up on a dropped tunnel.
                    raise RuntimeError(
                        f"aiui lost contact with the companion while waiting for "
                        f"render {render_id}: {_explain_exc(e)} "
                        f"({consecutive_failures} consecutive poll failures). "
                        f"The dialog may still be open on the Mac — check it "
                        f"before re-asking."
                    ) from e
                log.warning(
                    "poll %s failed (%s), retry %d/%d",
                    render_id, _explain_exc(e),
                    consecutive_failures, ASYNC_POLL_MAX_CONSECUTIVE_FAILURES,
                )
                await asyncio.sleep(ASYNC_POLL_RETRY_BACKOFF_S)
                continue
            consecutive_failures = 0
            if pr.status_code == 404:
                raise RuntimeError(
                    f"aiui lost track of render {render_id} (expired or never "
                    f"registered). Restart the dialog."
                )
            pr.raise_for_status()
            pv = pr.json()
            if pv.get("pending") is True:
                iteration += 1
                if ctx is not None:
                    # Best-effort: a missing progressToken or any reporting
                    # hiccup must never break the render.
                    try:
                        await ctx.report_progress(progress=float(iteration), total=None)
                    except Exception as e:  # noqa: BLE001
                        log.debug("progress report skipped: %s", _explain_exc(e))
                await asyncio.sleep(ASYNC_POLL_MIN_INTERVAL_S)
                continue
            return pv
    except asyncio.CancelledError:
        # `shield` is load-bearing: a bare `await` inside an `except
        # CancelledError` block is re-cancelled immediately on most loop states
        # and the DELETE never goes out. `BaseException`, not `Exception`,
        # because the shield itself re-raises `CancelledError`.
        try:
            await asyncio.shield(
                asyncio.wait_for(_cancel_render(render_id), CANCEL_RENDER_TIMEOUT_S)
            )
        except BaseException as e:  # noqa: BLE001
            log.debug("render cancel cleanup: %s", _explain_exc(e))
        raise


def _session_origin() -> str:
    """This bridge's host, auto-attached to every render as `session_origin`
    (Step 4, I8). The Mac can't tell remotes apart at the shared `:7777`, so
    the origin must come from the caller side — the user always sees which host
    a dialog came from even when the agent passes no `session` label."""
    try:
        return socket.gethostname()
    except OSError:
        return "remote"


async def _post_render(
    spec: dict[str, Any],
    ctx: Context | None = None,
    session: str | None = None,
) -> dict[str, Any]:
    await _wait_for_aiui()
    await _preflight()
    t0 = datetime.now(timezone.utc)
    log.info("render → kind=%s", spec.get("kind"))
    async with httpx.AsyncClient(timeout=TIMEOUT_S) as client:
        # Video first: push local video files to the Mac's /media cache and
        # swap their `src` for the returned playback URL — BEFORE the image
        # inliner runs, so it never tries to base64 a huge clip.
        media_warnings = await _upload_local_videos(spec, client)
        # Audio (#25): same reasoning — local audio for the form `audio`
        # field is routed through the /media cache instead of the 10 MB
        # `data:` inliner, uniformly regardless of clip size.
        media_warnings += await _upload_local_audios(spec, client)
        # Resolve any absolute / `~/`-rooted file paths *before* shipping
        # the spec down the HTTP wire. This bridge runs on the same host
        # as the agent — local for Mac use, remote for SSH-tunneled
        # remotes — so this is the only point in the chain where the
        # agent's filesystem actually exists. The Mac-side server resolver
        # only handles HTTPS.
        _resolve_local_paths(spec)
        # #199: the write for a `target` field happens on THIS host after
        # submit, so only this side knows where it lands. Resolve it into the
        # spec before the companion renders the approval line.
        _annotate_target_paths(spec)
        # Async render (Step 3): opt in via the header. A current companion
        # registers the dialog and answers immediately with `{id, ttl_secs}`
        # (202); we then poll for the result. An older companion ignores the
        # header and answers synchronously (200 with the terminal shape) — we
        # detect that and use it directly (backward-compatible).
        try:
            r = await client.post(
                f"{ENDPOINT}/render",
                headers={"Authorization": f"Bearer {_token()}", "x-aiui-async": "1"},
                json={
                    "spec": spec,
                    "session": session,
                    "session_origin": _session_origin(),
                },
            )
        except httpx.HTTPError as e:
            # #202: a blip during registration used to let a raw httpx
            # exception escape `_post_render` entirely — not routed through
            # `_explain_exc`, so a RemoteProtocolError (empty `str(e)`)
            # surfaced as the `error: ""` class of bug. Retrying the POST is
            # NOT the fix: it would open a second dialog window for the same
            # question. Fail with an explained error and let the agent decide.
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} failed to register the dialog: "
                f"{_explain_exc(e)}. No dialog was opened. On a remote this is "
                f"usually the SSH reverse-tunnel dropping — check aiui "
                f"Settings → Connections on the Mac, then retry."
            ) from e
        # #186/#182: a 422 from the companion carries `{error, detail, hint}`
        # — the whole point of the structured rejection. `raise_for_status`
        # discards the body, so a bridge-served agent used to get a bare
        # `HTTPStatusError: 422` and no idea what was wrong with its spec,
        # while a local Rust-bridge agent got the explanation. Surface it.
        if r.status_code == 422:
            try:
                body = r.json()
            except Exception:
                body = {}
            detail = body.get("detail") or "the companion rejected the dialog spec"
            hint = body.get("hint")
            raise RuntimeError(
                f"aiui rejected the dialog spec (invalid_spec): {detail}"
                + (f" — {hint}" if hint else "")
            )
        # #178: a spec past the companion's size ceiling — practically always
        # inlined images. Same `{error, detail, hint}` shape as the 422; the
        # hint is the whole point ("pass an http(s):// src instead"), so a bare
        # `HTTPStatusError: 413` would strip the only actionable part. Checked
        # before the generic `>= 400` arm below so it keeps its tailored hint.
        if r.status_code == 413:
            try:
                body = r.json()
            except Exception:
                body = {}
            detail = body.get("detail") or "the dialog spec is too large"
            hint = body.get("hint")
            raise RuntimeError(
                f"aiui rejected the dialog spec (spec_too_large): {detail}"
                + (f" — {hint}" if hint else "")
            )
        # #202: the remaining non-2xx statuses were left to `raise_for_status`,
        # which hands the agent a bare `HTTPStatusError` naming a URL and a
        # status code. Translate them into the same actionable style
        # `_preflight` uses, so a remote session gets the guidance a
        # Mac-local Rust-bridge session already gets.
        if r.status_code == 401:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} rejected our token (401) while "
                f"opening the dialog. The token was rotated mid-session, or "
                f"another aiui process is listening on this port. Re-register "
                f"this host from the companion's settings window on the Mac."
            )
        if r.status_code >= 500:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} failed to open the dialog "
                f"(HTTP {r.status_code}): {r.text[:200]}. This is a companion-side "
                f"fault — retry once; if it persists, restart aiui.app on the Mac."
            )
        if r.status_code >= 400:
            raise RuntimeError(
                f"aiui companion at {ENDPOINT} refused the render "
                f"(HTTP {r.status_code}): {r.text[:200]}"
            )
        first = r.json()
        if r.status_code == 202:
            render_id = first.get("id")
            if not isinstance(render_id, str) or not render_id:
                raise RuntimeError(
                    "aiui accepted the dialog (202) but its response carries no "
                    "`id`, so there is nothing to poll. The dialog may be open on "
                    "the Mac with no one listening — check it, and report this as "
                    "a companion bug."
                )
            ttl = first.get("ttl_secs")
            ttl_secs = (
                float(ttl)
                if isinstance(ttl, (int, float)) and not isinstance(ttl, bool) and ttl > 0
                else DEFAULT_POLL_TTL_S
            )
            data = await _poll_render(client, render_id, ctx, ttl_secs)
        else:
            data = first  # synchronous companion — terminal result already
    # Issue #135: this bridge runs ON the agent's host, so `target` fields are
    # written here as LOCAL file operations (the value arrived over the :7777
    # channel, never via the agent). Secret values are written and stripped
    # before the result is handed to the agent.
    _apply_target_writes(spec, data)
    # #194: a clip that never reached the media cache used to produce a
    # broken player for the user and no signal at all for the agent. The key
    # only appears when something actually failed — an always-present empty
    # list trains agents to skip it.
    if media_warnings:
        data["media_warnings"] = media_warnings
    dt = (datetime.now(timezone.utc) - t0).total_seconds()
    log.info(
        "render ← kind=%s cancelled=%s took=%.2fs",
        spec.get("kind"), data.get("cancelled"), dt,
    )
    return data


def _cancel_defaults(kind: str | None) -> dict[str, Any]:
    """The falsy keys a cancelled dialog still returns, per tool (#202).

    `_format_result` used to answer a bare `{"cancelled": True}`, contradicting
    every tool's own docstring — `confirm` promises `{cancelled, confirmed}`,
    `ask` promises `{cancelled, answers}`, `form` promises `{cancelled, values}`
    — and contradicting the Rust bridge, whose `format_confirm_result` always
    emits both keys. An agent following the documented shape and reading
    `result["confirmed"]` therefore worked on a Mac-local session and raised a
    `KeyError` only on a remote: a bridge-dependent bug invisible in local
    testing. `compare` is deliberately absent: `docs/skill.md` documents
    `selected` as *absent* on cancel, and inventing a falsy value there would
    read as a real selection.
    """
    if kind == "confirm":
        return {"confirmed": False}
    if kind == "ask":
        return {"answers": []}
    if kind == "form":
        return {"values": {}}
    if kind == "gallery":
        return {"decisions": {}}
    return {}


def _format_result(payload: dict[str, Any], kind: str | None = None) -> dict[str, Any]:
    if payload.get("cancelled"):
        out: dict[str, Any] = {"cancelled": True, **_cancel_defaults(kind)}
        # #180: forward WHY. The companion sets `host_exiting`,
        # `ttl_expired`, `evicted` and `channel_dropped`, but the bridge
        # flattened them all into a bare
        # `{"cancelled": true}` — indistinguishable from the user pressing
        # Escape. An agent that cannot tell "the user declined" from "the
        # companion was shutting down" retries the wrong thing.
        reason = payload.get("reason")
        if isinstance(reason, str) and reason:
            out["reason"] = reason
        _carry_media_warnings(payload, out)
        return out
    out = {"cancelled": False, **payload.get("result", {})}
    _carry_media_warnings(payload, out)
    return out


def _carry_media_warnings(payload: dict[str, Any], out: dict[str, Any]) -> None:
    """Forward `media_warnings` from the render payload onto the tool result
    (#194). Mirrors `carry_media_warnings` in the Rust bridge — and applies to
    the cancelled branch too: a user who cancels *because* the clip was a
    broken player is exactly the case the agent needs the warning for.
    """
    warnings = payload.get("media_warnings")
    if warnings:
        out["media_warnings"] = warnings


@mcp.tool()
async def ask(
    question: str,
    options: list[dict[str, Any]],
    header: str | None = None,
    multi_select: bool = False,
    allow_other: bool = False,
    session: str | None = None,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Before listing options in chat and waiting for the user to type back
    which one (deploy strategy, migration path, file to act on …), call
    this tool instead. Per-option `description` carries the trade-off;
    `multi_select` and `allow_other` cover the rest.

    WHEN TO USE: 2–6 mutually-exclusive options where per-option context helps.
    For yes/no, use `confirm`. For mixed inputs, use `form`.

    WRITE OPTIONS:
    - Label: noun or short imperative, ≤ 5 words, no punctuation, no emoji.
    - Description: one sentence stating the trade-off or consequence.
    - Keep options parallel in grammar.
    - For visual choice ("which of these images?") add `thumbnail` per
      option — same `src` rules as everywhere else (data: URL, http(s)
      URL, or absolute / `~/` local path on YOUR host). aiui resolves
      paths and URLs to data: URLs before render.

    ANTI-PATTERNS: > 8 options (use `form` with a `list` field); generic labels
    like "Option 1"; redundant descriptions that just restate the label.

    Returns `{cancelled, answers, other?}`. `answers` is a list of values.

    Args:
        question: Full question, imperative or interrogative.
        options: List of `{"label": str, "description"?: str, "value"?: str,
            "thumbnail"?: str}`.
        header: Short chip above the question (≤ 14 chars).
        multi_select: Allow selecting multiple options.
        allow_other: Offer a free-text fallback. Off by default — opt in when
            an answer you did not list is genuinely useful, because the reply
            then comes back in `other` instead of `answers`.
        session: Short human label for this session, shown in the window
            chrome so parallel dialogs stay distinguishable.
    """
    spec = {
        "kind": "ask",
        "question": question,
        "header": header,
        "options": options,
        "multiSelect": multi_select,
        "allowOther": allow_other,
    }
    return _format_result(await _post_render(spec, ctx, session), spec["kind"])


@mcp.tool()
async def form(
    title: str,
    fields: list[dict[str, Any]] | None = None,
    description: str | None = None,
    header: str | None = None,
    tabs: list[dict[str, Any]] | None = None,
    actions: list[dict[str, Any]] | None = None,
    submit_label: str | None = None,
    cancel_label: str | None = None,
    size: str | None = None,
    width: float | None = None,
    height: float | None = None,
    session: str | None = None,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Whenever the user needs to provide ≥ 2 related inputs, or any single
    input that doesn't belong in chat (secret, date/datetime/range,
    bounded number, sortable ranking, multi-select, color pick,
    table-row triage with column context, image confirm/grid), call
    this tool instead of typing the questions one by one.

    WHEN TO USE: ≥ 2 related inputs, or one input plus context/confirmation.
    For yes/no, use `confirm`. For a single choice, use `ask`.

    WRITE LABELS:
    - Imperative or noun, ≤ 6 words, no punctuation, no emoji.
    - Consistent register across all fields.
    - Field-level descriptions only if the label alone is ambiguous.

    BE RESTRAINT:
    - ≤ 8 fields per dialog. Split logically if you need more.
    - `static_text` only for context the user couldn't derive from labels.
    - Defaults that a human would actually pick.

    ACTION BUTTONS:
    - Verb-based and concrete ("Create report"), not "OK".
    - Styling variants (pick one per button):
      - `primary: true`  → blue, default emphasis for the main action.
      - `success: true`  → green, for positive-outcome actions ("Approve", "Publish", "Accept").
      - `destructive: true` → red, for deletions/force-pushes/rollbacks.
      - none → neutral outlined button.
      Never red a save button. Never green a delete button.
    - `skip_validation: true` on escape hatches so required-field validation
      doesn't trap the user.
    - ≤ 3 actions.

    FIELD KINDS:
    - text:        {kind, name, label, placeholder?, default?, multiline?, required?}
    - password:    {kind, name, label, placeholder?, required?}  — masked on screen only; value returns as plaintext in the response. Use for short-lived secrets; direct users to keychain/env for long-lived ones.
    - secret:      {kind, name, label, placeholder?, required?, target}  — masked input whose value is written to a file and NEVER returned to you (#135). Pair with `target` (see below). Use when the user must supply a credential that should not enter this conversation at all.
    - FILE-WRITE / `target` (any input field): add `target` to write the entered value to a file ON THE HOST YOU RUN ON when the user submits (the affirmative button is the per-write approval; the user sees the path first). Shape: `{"mode": "create"|"substitute", "path": "~/.github_tokens/byte5ai", "perm"?: "0600", "overwrite"?: bool, "placeholder"?: str}`. `create` writes the raw value (needs `overwrite:true` to clobber an existing file); `substitute` replaces a `placeholder` occurring exactly once in an existing file (format-agnostic: YAML/TOML/INI/…); choose a DISTINCTIVE sentinel that can't collide with real content (e.g. `__AIUI_SECRET_GITHUB_PAT__`, not a common word) — if it occurs 0 or >1 times the write is refused with an error, never misapplied. For a `secret` field the value is write-only (result: `{written, target, bytes}` — no value) and a `target` is REQUIRED: a target-less `secret` is rejected with `invalid_spec` instead of returning the plaintext — use `password` if you want the value back. A non-secret field with `target` is written AND returned. Only an affirmative action commits the write: the submit button or a plain named action — an action carrying `skip_validation:true` (Cancel / Save draft) writes nothing and returns `{written:false, error}` per field, unless you set `writes_targets:true` on it. A blank field writes nothing either (`refusing to write an empty value`), in both modes. Destination is always your own host: the aiui module on that host (this bridge for your session) writes it as a LOCAL file operation, so `create` and `substitute` both work identically whether you run locally or on a remote SSH host — a foreign host cannot be targeted. Errors: `{written:false, error}`.
    - number:      {kind, name, label, default?, min?, max?, step?, required?}
    - select:      {kind, name, label, options: [{label, value}], default?, required?}
    - checkbox:    {kind, name, label, default?}
    - slider:      {kind, name, label, min, max, step?, default?}
    - date:        {kind, name, label, default?, required?}  — ISO YYYY-MM-DD
    - datetime:    {kind, name, label, default?, required?}  — ISO YYYY-MM-DDTHH:MM
    - date_range:  {kind, name, label, default?: {from, to}, required?}  — result {from, to}
    - color:       {kind, name, label, default?}  — hex "#RRGGBB"
    - static_text: {kind, text, tone?: "info"|"warn"|"muted"}  — display only
    - markdown:    {kind, text}  — read-only Markdown block; only as inline context for following inputs in the same form, NOT as a standalone display tool.
    - image:       {kind, src, label?, alt?, max_height?}  — read-only image. `src` accepts an absolute / `~/` local path (read on YOUR host), an `http(s)://` URL (fetched on the Mac), or a `data:` URL. Use for visual confirmation of agent-generated previews.
    - annotated_image: {kind, name, src, label?, alt?, mode?, max_height?, required?, default?}  — let the user MARK a spot on an image (logo placement, crop hint, bug location). `src` follows the same rules as `image`. `mode` ∈ {"point" (click one marker, default), "region" (drag a rectangle), "both" (user flips a Point/Region tool)}. `default` may seed `{point?: {x, y}, region?: {x, y, w, h}}` in normalized units. Result under `name`: {point: {x, y} | null, region: {x, y, w, h} | null, natural: {width, height} | null} — all coordinates normalized 0..1; multiply by `natural` for pixels.
    - audio:       {kind, src, label?}  — read-only native `<audio controls>` player. Use for "listen to this TTS sample / voice memo / generated sound clip before deciding". `src` accepts a `data:audio/...` URL, an `http(s)://` URL, or an absolute/`~/` local path (mp3/m4a/wav/aac/ogg/flac) — local audio is pushed through the same size-unbounded `/media` cache as gallery video, never the 10 MB `data:` inliner.
    - mermaid:     {kind, source, label?, max_height?}  — read-only Mermaid diagram (flowchart, sequence, state, gantt, mindmap, …). `source` is a Mermaid-DSL string. Use this instead of ASCII / box-drawing art when you'd otherwise sketch a diagram in chat — aiui renders to SVG and DOMPurify-sanitises before display.
    - wireframe:   {kind, panels: [{title?, content?, col_span?, row_span?, tone?}], columns?, gap?, label?, max_height?}  — read-only UI-layout mockup. Real CSS-Grid panels with optional header (`title`) and multi-line monospace body (`content`, escape `\n`). `tone` ∈ {"default","muted","highlight"}. Use this for *UI-layouts* (dashboard tiles, hardware-UI panels, login screens, anything with fixed-position boxes-and-labels) instead of ASCII boxes-and-pipes — `mermaid` is for *diagrams* (graphs, sequence/state, gantt). Wireframe complements it for the layout class.
    - image_grid:  {kind, name, label?, images: [{value, src, label?}], multi_select?, columns?, default_selected?, required?}
      Result: {selected: [values]}
    - list:        {kind, name, label?, items: [{label, value, description?, thumbnail?}],
                    selectable?, multi_select?, sortable?, default_selected?: [values]}
      Result: {selected: [values], order: [values]}. Thumbnails optional per item.
    - table:       {kind, name, label?, columns: [{key, label, align?}], rows: [{value, values}],
                    multi_select?, sortable_by_column?, default_selected?, required?}
      Result: {selected: [values], order: [values], sort: {column, dir}}
    - tree:        {kind, name, label?, items: [{label, value, description?, children?: [...]}],
                    multi_select?, default_selected?: [values], default_expanded?: [values]}
      Result: {selected: [values]}

    TABS (optional grouping for long forms):
    Pass `tabs=[{"label": ..., "fields": [...]}, ...]` instead of (or alongside,
    but `tabs` wins) `fields`. One submit covers all tabs; validation jumps to
    the first invalid tab. Tabs structure presentation only — they are not a
    wizard, no per-tab confirmation.

    Returns `{cancelled, action?, values: {name: value, ...}}`.

    Args:
        title: Window title. Same rules as labels.
        fields: List of field blocks, each with a `kind` from above. Use this
            OR `tabs`, not both.
        description: Subtitle, ≤ 2 sentences.
        header: Chip above the title (≤ 14 chars).
        tabs: Tab-grouped field list `[{label, fields}]` for longer forms.
        actions: Footer buttons `[{label, value, primary?, success?, destructive?, skip_validation?}]`.
            Styling variants are mutually exclusive; pick one of primary/success/destructive or leave all off for neutral.
            Without actions, defaults to Cancel + Submit.
        submit_label: Legacy fallback for the default submit button label.
        cancel_label: Legacy fallback for the default cancel button label.
        size: Starting window size hint — "s", "m", or "l". aiui picks good
            local defaults and clamps to the screen. The window is always
            resizable; this only sets the *initial* size and never opens
            smaller than the content needs. Use "m"/"l" for forms with
            images, tables, wireframes, or many fields so they don't open
            cramped.
        width: Explicit starting width in logical px (overrides `size`).
            Rarely needed — prefer `size`.
        height: Explicit starting height in logical px (overrides `size`).
            Rarely needed — prefer `size`.
        session: Short human label for this session, shown in the window
            chrome so parallel dialogs stay distinguishable.
    """
    spec = {
        "kind": "form",
        "title": title,
        "description": description,
        "header": header,
        "fields": fields,
        "tabs": tabs,
        "actions": actions,
        "submitLabel": submit_label,
        "cancelLabel": cancel_label,
        "size": size,
        "width": width,
        "height": height,
    }
    return _format_result(await _post_render(spec, ctx, session), spec["kind"])


@mcp.tool()
async def confirm(
    title: str,
    message: str | None = None,
    header: str | None = None,
    destructive: bool = False,
    confirm_label: str | None = None,
    cancel_label: str | None = None,
    image: dict[str, Any] | None = None,
    session: str | None = None,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Before writing any yes/no question into chat, call this tool instead.
    Pass `destructive=True` (red button) for delete / drop / force-push /
    rollback / prod-deploy — never trust loose prior approval for
    irreversible steps; re-confirm in a dialog.

    WHEN TO USE: irreversible or high-stakes step where "just proceed" is
    unsafe. For pure information, respond in chat. For 3+ options, use `ask`.
    For visual sign-off ("is this generated image OK?"), pass `image`.

    WRITE:
    - Title: the decision as a question, ≤ 10 words.
    - Message: one sentence stating the concrete consequence.
    - `destructive=True` for deletions/force-pushes/rollbacks — never for
      saves or creates.
    - Custom `confirm_label`/`cancel_label` when verbs clarify.
    - `image` for visual confirmation — same `src` rules as elsewhere
      (data: URL, http(s) URL, or absolute / `~/` local path on YOUR host).

    Returns `{cancelled, confirmed}`. `cancelled=True` means Escape or window
    close. `cancelled=False, confirmed=False` means the explicit No button.

    Args:
        title: The decision phrased as a question.
        message: One-sentence explanation of what happens on confirm.
        header: Chip above the title.
        destructive: Red confirm button.
        confirm_label: Defaults to the companion's localized affirmative
            label — resolved from the user's locale, so do not name it in
            chat unless you set it yourself.
        cancel_label: Defaults to the companion's localized negative label,
            same rule.
        image: `{"src": str, "alt"?: str, "max_height"?: int}`. Shown above
            the title for visual confirmation. `src` follows the standard
            aiui resolution rules.
        session: Short human label for this session, shown in the window
            chrome so parallel dialogs stay distinguishable.
    """
    spec = {
        "kind": "confirm",
        "title": title,
        "message": message,
        "header": header,
        "destructive": destructive,
        "confirmLabel": confirm_label,
        "cancelLabel": cancel_label,
        "image": image,
    }
    return _format_result(await _post_render(spec, ctx, session), spec["kind"])


@mcp.tool()
async def gallery(
    items: list[dict[str, Any]],
    title: str | None = None,
    description: str | None = None,
    header: str | None = None,
    actions: list[dict[str, Any]] | None = None,
    comment: bool = False,
    columns: int | None = None,
    submit_label: str | None = None,
    cancel_label: str | None = None,
    size: str | None = None,
    width: float | None = None,
    height: float | None = None,
    session: str | None = None,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Batch visual review: show several images and/or videos at once and
    collect a per-item decision (+ optional comment) in ONE window, instead
    of calling `confirm` once per asset.

    WHEN TO USE: "review these N generated images", "triage this batch of
    screenshots", "approve/revise/skip each of these renders". For a single
    image sign-off use `confirm` with `image`; for a one-of-N choice use
    `ask` with thumbnails.

    Each item needs a stable `value` (the key you get the decision back
    under) and usually a `src`. `src` follows the standard aiui resolution
    rules (data: URL, http(s) URL, or absolute / `~/` local path on YOUR
    host). Videos are detected by `data:video/` MIME or a
    .mp4/.mov/.m4v/.webm extension and render with native controls.

    Per-item buttons come from `actions` (default Approve / Revise / Skip).
    Set `comment=True` for a free-text field per item.

    Returns `{cancelled, decisions}` where `decisions` maps each touched
    item's `value` to `{decision, comment?}`. Items the user didn't touch
    are omitted.

    Args:
        items: List of `{value, src?, alt?, label?, detail?, max_height?}`.
            `value` must be non-empty and unique. Order is preserved.
        title: What the user is reviewing, e.g. "Review 6 hero renders".
        description: One sentence of context under the title.
        header: Chip above the title (≤ 14 chars).
        actions: Per-item decision buttons as
            `[{label, value, primary?, success?, destructive?}]`. Defaults
            to Approve (green) / Revise / Skip.
        comment: Show a free-text comment field per item.
        columns: Grid columns. Omit for responsive auto-fill.
        submit_label: Footer submit button label.
        cancel_label: Footer cancel button label.
        size: Starting window size hint — "s", "m", or "l". Defaults to
            auto-sizing by item count; pass "l" for a large batch or tall
            thumbnails so the grid opens roomy. Always resizable; never opens
            smaller than the content needs.
        width: Explicit starting width in logical px (overrides `size`).
        height: Explicit starting height in logical px (overrides `size`).
        session: Short human label for this session, shown in the window
            chrome so parallel dialogs stay distinguishable.
    """
    spec = {
        "kind": "gallery",
        "title": title,
        "description": description,
        "header": header,
        "items": items,
        "actions": actions,
        "comment": comment,
        "columns": columns,
        "submitLabel": submit_label,
        "cancelLabel": cancel_label,
        "size": size,
        "width": width,
        "height": height,
    }
    return _format_result(await _post_render(spec, ctx, session), spec["kind"])


@mcp.tool()
async def upload(
    target_dir: str | None = None,
    session: str | None = None,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Pull a file FROM the user's Mac INTO this agent session.

    Calling this opens a native file picker on the user's Mac; the file they
    choose is streamed back over aiui's channel and written to `target_dir` on
    YOUR host (the machine you run on — the remote for an SSH session). This is
    the counterpart to the user having to `scp` a file over: reach for it
    whenever the user says "take this file", "upload …", "here's the
    file/screenshot/PDF", or triggers `/aiui:upload`.

    WHEN TO USE: the user wants to give you a local Mac file. Do NOT ask them
    which file — they pick it in the native dialog. Do NOT ask where to put it;
    infer `target_dir` from the conversation (usually your cwd or the active
    project dir) and pass it.

    BEHAVIOUR:
    - `target_dir` is optional but you should almost always pass it — an
      absolute or `~/`-rooted directory ON YOUR HOST. Omit it only when you
      have no context (defaults to your process's cwd). Relative paths are
      rejected. The directory must already exist and be writable.
    - The filename comes from the user's selection; the file lands at
      `target_dir/<filename>` — a deterministic path, no temp/staging dir.
    - Existing files are never overwritten: if `target_dir/<filename>` already
      exists the call errors instead of clobbering. Pick a different
      `target_dir` or move the old file first.
    - Blocks until the user picks a file or dismisses the picker. Progress
      notifications fire every ~10 s meanwhile — a slow response just means the
      user is choosing a file, not that aiui is broken. The picker may take a
      moment to come forward.
    - Only one picker at a time: a concurrent `upload` errors with "another
      upload is already waiting" — let that one finish, then retry. An
      unanswered picker times out with an error rather than hanging forever.

    Returns `{status: "ok", path, filename, bytes}` on success, or
    `{status: "error", error}` on any failure (user cancelled, file unreadable,
    file too large — 512 MB cap, target directory missing/not writable, another
    upload in flight, picker never answered). Report briefly; on `ok`, mention
    the path the file landed at.

    Args:
        target_dir: Absolute or `~/`-rooted directory on your host where the
            picked file is written as `<target_dir>/<filename>`. Defaults to
            your process's cwd.
        session: Short human label for this session, shown in the file
            picker's title bar so the user can tell which agent asked for a
            file.
    """
    # Resolve the destination up front so a bad target_dir fails before the
    # picker even opens (nothing worse than picking a file only to be rejected).
    if target_dir is not None and target_dir.strip():
        dest_dir = _upload_expand_dir(target_dir.strip())
        if dest_dir is None:
            return {
                "status": "error",
                "error": f"target_dir must be an absolute or ~/-rooted path, got '{target_dir}'",
            }
    else:
        # #194: `Path.cwd()` raises when the process's working directory has
        # been removed — an exception escaping `upload` breaks its contract
        # that every failure comes back as `{status: "error", error}`. The
        # Rust bridge already answered "no target_dir given and cwd
        # unavailable"; say the same thing here.
        try:
            dest_dir = Path.cwd()
        except (OSError, RuntimeError) as e:
            return {"status": "error", "error": f"no target_dir given and cwd unavailable: {e}"}
    if not dest_dir.is_dir():
        return {"status": "error", "error": f"target directory does not exist: {dest_dir}"}

    await _wait_for_aiui()
    await _preflight()

    heartbeat = asyncio.create_task(_upload_heartbeat(ctx)) if ctx is not None else None
    try:
        async with httpx.AsyncClient(timeout=UPLOAD_TIMEOUT_S) as client:
            r = await client.post(
                f"{ENDPOINT}/upload",
                headers={"Authorization": f"Bearer {_token()}"},
                # #194: the optional body titles the picker, so a user facing
                # several agents can tell which one is asking. Additive — an
                # older companion has no body extractor and ignores it.
                json={"session": session},
            )
    except httpx.HTTPError as e:
        return {"status": "error", "error": f"POST /upload failed: {_explain_exc(e)}"}
    finally:
        if heartbeat is not None:
            heartbeat.cancel()

    if r.status_code == 204:
        return {"status": "error", "error": "upload cancelled — no file was selected"}
    if r.status_code == 409:
        # #194: the companion serialises the native picker — two stacked
        # system panels are indistinguishable to the user.
        return {
            "status": "error",
            "error": (
                "another upload is already waiting for the user — one file picker at a "
                "time. Wait for that one to be answered, then retry."
            ),
        }
    if r.status_code == 504:
        # #194: the companion's own 600 s bound fires before this bridge's
        # 900 s, so its diagnosis wins the race over a generic timeout.
        return {
            "status": "error",
            "error": (
                "the file picker was never answered — it timed out on the user's "
                "machine. Ask the user whether the picker appeared, then retry."
            ),
        }
    if r.status_code == 413:
        return {"status": "error", "error": f"selected file too large: {r.text[:200]}"}
    if r.status_code != 200:
        return {
            "status": "error",
            "error": f"companion /upload failed ({r.status_code}): {r.text[:200]}",
        }

    # Filename travels in a percent-encoded header; the body is the raw bytes.
    raw_header = r.headers.get("x-aiui-filename", "")
    decoded = urllib.parse.unquote_to_bytes(raw_header).decode("utf-8", "replace")
    filename = _upload_safe_base_name(decoded) or "upload.bin"
    data = r.content
    if len(data) > UPLOAD_FILE_CAP:
        return {"status": "error", "error": f"uploaded file exceeds cap: {len(data)} bytes"}

    result = _upload_write(dest_dir, filename, data)
    log.info("upload ← filename=%s bytes=%d status=%s", filename, len(data), result.get("status"))
    return result


@mcp.tool()
async def compare(
    variants: list[dict[str, Any]],
    title: str | None = None,
    description: str | None = None,
    header: str | None = None,
    sync_scroll: bool = False,
    columns: int | None = None,
    submit_label: str | None = None,
    cancel_label: str | None = None,
    size: str | None = None,
    width: float | None = None,
    height: float | None = None,
    session: str | None = None,
    ctx: Context | None = None,
) -> dict[str, Any]:
    """Side-by-side A/B (or A/B/C) compare: render 2+ variants as full-content
    panes next to each other and let the user click ONE to pick.

    WHEN TO USE: "which draft is better", "which image edit", "before vs.
    after", "which of these three headlines". Use this instead of `ask` with
    `thumbnail` (which only shows a small icon per option, not the full
    content) or `gallery` (per-item batch review — approve/revise/skip each,
    not a single pick).

    Each variant needs a stable `value` (the key returned as `selected`) and
    at least one of `content` (markdown text — drafts, diffs, code) or `src`
    (image/video, standard aiui resolution rules: data: URL, http(s) URL, or
    absolute / `~/` local path on YOUR host; videos render with native
    controls). A variant may carry both — e.g. an image plus a caption.

    Returns `{cancelled, selected}` — `selected` is the `value` of the picked
    variant (only set when the user actually submits).

    Args:
        variants: List of `{value, label?, content?, src?, alt?, detail?,
            max_height?}`. Needs at least 2 entries (A/B) — 3 for A/B/C.
            `value` must be non-empty. `label` defaults to A / B / C / … by
            position. `max_height` set on ANY variant caps every pane's
            height so they stay equal-height and read as side-by-side.
        title: What's being compared, e.g. "Which intro paragraph?".
        description: One sentence of context under the title.
        header: Chip above the title (≤ 14 chars).
        sync_scroll: Lock scroll position across all panes — useful when
            comparing long text side by side.
        columns: Override the number of columns. Defaults to
            `len(variants)`, capped at 4.
        submit_label: Footer submit button label.
        cancel_label: Footer cancel button label.
        size: Starting window size hint — "s", "m", or "l". Default
            auto-sizes to variant count and content. Always resizable;
            never opens smaller than the content needs.
        width: Explicit starting width in logical px (overrides `size`).
        height: Explicit starting height in logical px (overrides `size`).
        session: Short human label for this session, shown in the window
            chrome so parallel dialogs stay distinguishable.
    """
    spec = {
        "kind": "compare",
        "title": title,
        "description": description,
        "header": header,
        "variants": variants,
        "syncScroll": sync_scroll,
        "columns": columns,
        "submitLabel": submit_label,
        "cancelLabel": cancel_label,
        "size": size,
        "width": width,
        "height": height,
    }
    return _format_result(await _post_render(spec, ctx, session), spec["kind"])


@mcp.tool()
async def notify(
    title: str,
    body: str,
    subtitle: str | None = None,
    sound: str | None = None,
) -> dict[str, Any]:
    """Fire a native macOS notification and return immediately — use this
    for an async-completion signal to a user who isn't watching this
    session ("tests green", "deploy finished", "merge conflicts, need
    you"). Unlike `confirm`/`ask`/`form`/`gallery`, this tool does NOT wait
    for the user: it hands the notification to the OS and returns
    `{ok: True}` right away, with no dialog, no window, no response to
    parse.

    WHEN TO USE: the point is exactly that the user doesn't have to be
    looking at this session to notice — a long-running task just finished,
    something needs their attention whenever they get to it. Use it
    instead of a chat message for that case.

    WHEN NOT TO USE: anything that needs an answer (yes/no, a choice,
    input) — `notify` has no way to carry a reply back. Use `confirm`,
    `ask`, or `form` instead.

    Runs against the *user's Mac*, regardless of whether this MCP is local
    or reached via an SSH reverse-tunnel — same as `update`/`version`,
    the notification always renders on the Mac side.

    Returns `{ok: bool, error?: str}`. `ok: False` most commonly means the
    user hasn't granted aiui notification permission on macOS yet (the OS
    prompts for this once, on the first `notify` call) — not a bug to
    retry around.

    Args:
        title: Short headline, ≤ ~40 chars — notification banners
            truncate longer text.
        body: The detail — what finished, what needs attention.
        subtitle: Optional extra context line.
        sound: Optional OS notification sound name (e.g. "default").
            Omit for silent.
    """
    # Cold-start gate, same as the render path (#203). `notify` is by
    # definition called when a long task finishes — the moment the tunnel is
    # most likely to have just been re-established — so failing instantly
    # against a companion that is still starting is exactly the wrong trade.
    await _wait_for_aiui()
    try:
        async with httpx.AsyncClient(timeout=TIMEOUT_S) as client:
            r = await client.post(
                f"{ENDPOINT}/notify",
                headers={"Authorization": f"Bearer {_token()}"},
                json={"title": title, "body": body, "subtitle": subtitle, "sound": sound},
            )
            if r.status_code == 422:
                # Structured invalid_request from the companion (e.g. empty
                # title) — surface detail so the agent can fix the call.
                try:
                    detail = r.json().get("detail", r.text)
                except ValueError:
                    detail = r.text
                raise RuntimeError(f"aiui rejected the notification: {detail}")
            r.raise_for_status()
            return r.json()
    except RuntimeError:
        raise  # our own structured error above — pass through verbatim
    except Exception as e:
        raise RuntimeError(
            f"aiui /notify failed at {ENDPOINT}: {_explain_exc(e)}. "
            f"Run `aiui_health` first to check whether aiui.app is reachable."
        ) from e


@mcp.prompt(name="teach")
def teach_prompt() -> str:
    """Brief the agent on aiui. Loads the full widget catalog, design
    rules, and anti-patterns into the session. Run once per project so
    the agent reaches for the right dialog without further prompting."""
    try:
        return (resources.files("aiui_mcp") / "skill.md").read_text()
    except Exception:
        return (
            "aiui skill doc not bundled with this install. "
            "See https://github.com/byte5ai/aiui/blob/main/docs/skill.md"
        )


# Prompt texts kept in sync verbatim with the Rust MCP server
# (companion/src-tauri/src/mcp.rs) so /aiui:update and /aiui:version behave
# identically whether the user is on the native app MCP or on PyPI via uvx.

_UPDATE_PROMPT = """\
Check whether an aiui update is available and install it if so. Call the \
`update` tool now, then report back concisely:

- If `updated: true`, report "aiui updated {current} -> {available}" and \
  mention that aiui will relaunch itself silently; the next agent call \
  will hit the new version.
- If `updated: false` and `note: "already on latest"`, report "aiui is \
  on the latest version ({current})".
- If `updated: false` and `note` mentions a dialog in flight, report that \
  aiui {available} is ready but was not installed because a dialog is \
  still open on the user's machine — installing would close it and \
  discard what they typed. Ask them to finish it, then run /aiui:update \
  again. Do not retry on your own.
- If `error` is set, report the error verbatim.

Keep the reply to one short sentence unless the user asked for detail.
"""

_VERSION_PROMPT = """\
Report the current aiui version to the user. Call the `version` tool and \
reply with one short line containing the version plus the build date \
parsed from `build_info` (format "v{ver} (commit, yyyy-mm-dd)"). If the \
user asked for more, include the binary path and updater endpoint.
"""


@mcp.prompt()
def update() -> str:
    """Instructs the agent to call `update` and report the outcome.

    Wired up so Claude Code exposes `/aiui:update` as a slash-command that
    triggers a silent update check + install on the user's Mac. Works both
    locally (MCP talks to aiui on localhost) and remotely (MCP calls reach
    aiui through the SSH reverse-tunnel — the update runs on the user's Mac,
    not on the remote host)."""
    return _UPDATE_PROMPT


@mcp.prompt()
def version() -> str:  # noqa: A001  — shadowing by design; prompt names surface as `/aiui:version`
    """Instructs the agent to call `version` and report the current aiui
    companion version in a single line."""
    return _VERSION_PROMPT


_HEALTH_PROMPT = """\
Run the `aiui_health` tool and report the result in one short sentence:

- If `ready: true`, say "aiui ready (v{version})".
- If `ready: false`, read `reason` and `hint` from the response body and \
  relay the `hint` — it already names the cause and the one-step fix, with \
  the live numbers filled in. Don't guess a cause the body doesn't state. \
  Only if `hint` is absent, fall back to "restart aiui".
- `ready: false` with `reason: "dialog_registry_full"` or \
  `"too_many_children"` is degraded, not down: say so, because dialogs \
  still render.

Don't dump the raw JSON unless the user asked for it.
"""

_TEST_DIALOG_PROMPT = """\
Open a small demo dialog so the user can verify aiui is wired up end to \
end. Call the `confirm` tool with:

  title: "aiui test dialog"
  message: "Click any button — this just verifies the wiring."
  header: "Demo"
  confirm_label: "It works"
  cancel_label: "Close"

Report the outcome in one line: "aiui ok — you clicked '{label}'" if the \
window opened and returned, or the underlying error if it didn't.
"""

_REMOTES_PROMPT = """\
Show the user a quick rundown of their registered aiui remotes — same set \
the Settings window's "Eingerichtete Remote-Hosts" section shows, but in \
chat. Call `aiui_health` first to confirm aiui is up; if it isn't, just \
tell the user that and stop. Otherwise read `remotes.json` from aiui's \
config directory — `~/.config/aiui/` on macOS and Linux, \
`%APPDATA%\\aiui\\` on Windows (JSON array of host strings) — and present \
the entries in a compact list. If the file is missing or empty, say "no \
remotes registered yet — open Settings to add one".
"""


@mcp.prompt(name="health")
def health_prompt() -> str:
    """Instructs the agent to call `aiui_health` and report the result in
    one short sentence. Surfaces as `/aiui:health` in Claude Code."""
    return _HEALTH_PROMPT


@mcp.prompt(name="test-dialog")
def test_dialog_prompt() -> str:
    """Demo dialog so the user can verify aiui is wired up end to end.
    Surfaces as `/aiui:test-dialog` in Claude Code."""
    return _TEST_DIALOG_PROMPT


@mcp.prompt(name="remotes")
def remotes_prompt() -> str:
    """Quick rundown of registered aiui remotes in chat (same set the
    Settings window shows). Surfaces as `/aiui:remotes` in Claude Code."""
    return _REMOTES_PROMPT


_UPLOAD_PROMPT = """\
Call the `upload` tool to let me hand you a file from my Mac. \
Use my current working directory as the target unless I say otherwise.
"""


@mcp.prompt(name="upload")
def upload_prompt() -> str:
    """Hand a file from the Mac to the agent session — opens a native file
    picker and writes the chosen file to the agent host. Surfaces as
    `/aiui:upload` in Claude Code."""
    return _UPLOAD_PROMPT


@mcp.tool()
async def aiui_health() -> dict[str, Any]:
    """Reachability + token check against the aiui companion.

    Use this first if dialogs hang or fail — it distinguishes a cold companion
    (user needs to launch Claude Desktop, or the SSH tunnel is down) from a
    rogue local process holding the port with the wrong token.

    The companion's body is returned on *any* status: a 503 carries the
    ``reason``, ``hint``, ``pending``, ``oldest_age_secs`` and
    ``lifecycle_phase`` that are the whole point of the composite response, and
    `raise_for_status()` used to throw exactly that diagnosis away (#179).
    ``ok`` reports whether the companion answered 200, so a degraded-but-serving
    companion comes back as ``ok: true`` with ``ready: false``.
    """
    # Deliberately NOT gated by `_wait_for_aiui` (#203). Every other tool
    # waits out a cold start; this one is the diagnostic and must answer fast
    # — spending COLDSTART_WAIT_S before reporting "unreachable" would make
    # the tool people run *because* things hang hang too. Do not "fix" this.
    try:
        async with httpx.AsyncClient(timeout=HEALTH_TIMEOUT_S) as client:
            r = await client.get(
                f"{ENDPOINT}/health",
                headers={"Authorization": f"Bearer {_token()}"},
            )
            data = _health_body(r)
            if data is None:
                return {
                    "ok": False,
                    "error": (
                        f"/health answered HTTP {r.status_code} with a non-JSON body: "
                        f"{r.text[:200]}"
                    ),
                    "endpoint": ENDPOINT,
                    "server": BUILD_INFO,
                }
            out: dict[str, Any] = {
                "ok": r.status_code == 200,
                **data,
                "endpoint": ENDPOINT,
                "server": BUILD_INFO,
            }
            if r.status_code != 200:
                # Keep the status legible now that it is no longer the whole
                # answer — a 401 body is just `{"error": "unauthorized"}`.
                out["http_status"] = r.status_code
            return out
    except Exception as e:
        log.warning("health check failed: %s", e)
        return {
            "ok": False,
            "error": _explain_exc(e),
            "endpoint": ENDPOINT,
            "server": BUILD_INFO,
        }


# Renamed from the FastMCP-decorator `version` prompt — tools use a
# differently-scoped namespace, so no collision, but this aliasing makes the
# intent explicit in logs.
@mcp.tool(name="version")
async def version_tool() -> dict[str, Any]:
    """Report aiui companion version, build info, binary path, and updater endpoint.

    Cheap; does not hit the network. Works against both a local companion
    (on-Mac) and a remote one reached via SSH tunnel.
    """
    await _wait_for_aiui()  # cold-start gate, as on every other tool (#203)
    try:
        async with httpx.AsyncClient(timeout=HEALTH_TIMEOUT_S) as client:
            r = await client.get(
                f"{ENDPOINT}/version",
                headers={"Authorization": f"Bearer {_token()}"},
            )
            r.raise_for_status()
            return r.json()
    except Exception as e:
        # Same defensive wrapping as aiui_health: a bare exception with an
        # empty message would surface as "Error executing tool version:" in
        # the client and leave the user with nothing to act on. Mirror the
        # diagnosis aiui_health gives.
        raise RuntimeError(
            f"aiui /version failed at {ENDPOINT}: {_explain_exc(e)}. "
            f"Run `aiui_health` for a full reachability diagnosis."
        ) from e


@mcp.tool(name="update")
async def update_tool() -> dict[str, Any]:
    """Check for an aiui update on the user's machine and install it.

    Responds BEFORE the companion goes away, so the caller receives
    `{updated, current, available, note}`. Next agent call hits the new
    version. Two outcomes are not installs and need no retry logic:
    `updated: false` with `note: "already on latest"`, and `updated: false`
    with `note: "dialog in flight — update deferred"` — the companion
    refuses to restart out from under a dialog the user is filling in, so
    ask them to finish it and call again.

    Runs the updater against the *user's machine*, regardless of whether the
    MCP is local or reached via an SSH reverse-tunnel — because the
    /update HTTP endpoint lives on the aiui companion, not on this process.
    """
    # Cold-start gate (#203): `update` is the tool a user reaches for right
    # after restarting things, i.e. against a companion that is still coming
    # up.
    await _wait_for_aiui()
    # Use the long render timeout because download + install of the updater
    # bundle can take several seconds on a slow network.
    try:
        async with httpx.AsyncClient(timeout=TIMEOUT_S) as client:
            r = await client.post(
                f"{ENDPOINT}/update",
                headers={"Authorization": f"Bearer {_token()}"},
            )
            r.raise_for_status()
            return r.json()
    except Exception as e:
        raise RuntimeError(
            f"aiui /update failed at {ENDPOINT}: {_explain_exc(e)}. "
            f"Run `aiui_health` first to check whether aiui.app is reachable."
        ) from e


def main() -> None:
    """Entry point for the `aiui-mcp` console script. Default transport is
    stdio (what Claude Code expects). Legacy `--stdio` flag is accepted for
    compatibility with the old script-based invocation."""
    # stdio is the only transport we support; flag-parsing kept minimal.
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
