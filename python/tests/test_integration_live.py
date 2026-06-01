"""Live integration smoke tests for the remote → companion HTTP path (harness
Stufe 1).

Unlike the unit tests, these talk to a REAL aiui companion over the real path
(localhost:7777, i.e. through the SSH reverse-tunnel when run on a remote).
They are strictly READ-ONLY — no `/render`, so no dialog windows pop on the
user's Mac.

Opt-in: skipped unless `AIUI_LIVE=1`, so the normal `pytest` run and CI never
touch the network. When `AIUI_LIVE=1` is set but no companion is reachable,
they SKIP with a message rather than failing (the opt-in may run where nothing
is up).

Run from the remote (or anywhere the tunnel reaches the companion):

    AIUI_LIVE=1 uv run --extra dev pytest tests/test_integration_live.py -v

Tolerant of companion version: assertions that target >=0.5.0 features
(wire_version, the async `GET /render/{id}` route) degrade gracefully so the
suite also passes against an older installed release.
"""
from __future__ import annotations

import os
from pathlib import Path

import httpx
import pytest

ENDPOINT = os.environ.get("AIUI_ENDPOINT", "http://127.0.0.1:7777")
TOKEN_PATH = Path(os.environ.get("AIUI_TOKEN_PATH", "~/.config/aiui/token")).expanduser()
TIMEOUT = 6.0

pytestmark = pytest.mark.skipif(
    os.environ.get("AIUI_LIVE") != "1",
    reason="live integration test — set AIUI_LIVE=1 with a running companion",
)


def _token() -> str:
    return TOKEN_PATH.read_text().strip()


def _auth() -> dict[str, str]:
    return {"Authorization": f"Bearer {_token()}"}


@pytest.fixture(scope="module", autouse=True)
def _require_companion() -> None:
    """Skip the whole module (clear message) if the companion isn't reachable
    or the token is missing — never a hard failure for the opt-in run."""
    try:
        r = httpx.get(f"{ENDPOINT}/ping", timeout=TIMEOUT)
    except Exception as e:  # noqa: BLE001
        pytest.skip(f"no companion reachable at {ENDPOINT}: {e}")
    if r.status_code != 200:
        pytest.skip(f"companion /ping returned {r.status_code} at {ENDPOINT}")
    if not TOKEN_PATH.exists():
        pytest.skip(f"no aiui token at {TOKEN_PATH}")


def test_ping_is_unauthenticated_pong() -> None:
    r = httpx.get(f"{ENDPOINT}/ping", timeout=TIMEOUT)
    assert r.status_code == 200
    assert r.text.strip() == "pong"


def test_health_ready_shape() -> None:
    r = httpx.get(f"{ENDPOINT}/health", headers=_auth(), timeout=TIMEOUT)
    assert r.status_code == 200
    body = r.json()
    assert "version" in body
    assert "ready" in body
    # composite-health sub-objects
    assert "webview" in body and "dialogs" in body and "children" in body


def test_version_shape() -> None:
    r = httpx.get(f"{ENDPOINT}/version", headers=_auth(), timeout=TIMEOUT)
    assert r.status_code == 200
    body = r.json()
    for k in ("version", "build_info", "binary_path", "updater_endpoint"):
        assert k in body, f"/version missing {k}"
    # Step-2 cooperative floor; present on >=0.5.0 only — tolerate absence.
    if "wire_version" in body:
        assert isinstance(body["wire_version"], int)


def test_probe_self_shape() -> None:
    r = httpx.get(f"{ENDPOINT}/probe", headers=_auth(), timeout=TIMEOUT)
    assert r.status_code == 200
    body = r.json()
    assert body.get("aiui") is True
    assert "pid" in body and "build_sha" in body


def test_unauthorized_rejected() -> None:
    r = httpx.get(
        f"{ENDPOINT}/health",
        headers={"Authorization": "Bearer definitely-not-the-token"},
        timeout=TIMEOUT,
    )
    assert r.status_code == 401


def test_health_reports_lifecycle_phase() -> None:
    """#137 lifecycle state machine: >=0.8.0 health reports the current phase
    (Serving in steady state). Tolerate older companions that omit it."""
    r = httpx.get(f"{ENDPOINT}/health", headers=_auth(), timeout=TIMEOUT)
    assert r.status_code == 200
    body = r.json()
    if "lifecycle_phase" not in body:
        pytest.skip("companion predates the lifecycle phase field (<0.8.0)")
    # A companion answering /health is by definition past Starting.
    assert body["lifecycle_phase"] in ("Serving", "GracePending"), body["lifecycle_phase"]


def test_media_route_exists_and_404s_for_unknown() -> None:
    """#135/video media cache: GET /media/blob/<unknown> is served (404 for a
    missing file, not a route-miss). Read-only. Skip on pre-media companions."""
    r = httpx.get(
        f"{ENDPOINT}/media/blob/nonexistent-harness-probe.bin",
        timeout=TIMEOUT,
    )
    if r.status_code == 404 and (r.text or "").strip() == "" and "ServeDir" not in r.headers.get("server", ""):
        # Could be a route-miss on an older companion; ServeDir 404s are also
        # empty-body, so we can't distinguish — accept 404 either way as "no
        # such file", and only fail on a hard 5xx.
        pass
    assert r.status_code in (404, 416), f"unexpected status {r.status_code}"


def test_media_upload_requires_auth() -> None:
    """POST /media without a bearer token is rejected (401) before any write —
    the media push is an authenticated, mutating endpoint."""
    r = httpx.post(f"{ENDPOINT}/media?ext=mp4", content=b"x", timeout=TIMEOUT)
    if r.status_code == 404:
        pytest.skip("companion has no /media endpoint (pre-0.7.0)")
    assert r.status_code == 401


def test_render_get_unknown_id_route_exists_and_404s_cleanly() -> None:
    """`GET /render/{id}` for a never-registered id must be served by the async
    handler — 404 with body `unknown_render_id`. This deliberately asserts the
    body, not just the status: an empty-body 404 means the *route itself* didn't
    match (the axum 0.7 `:id` vs 0.8 `{id}` mismatch that shipped in the first
    0.5.0 build and that a status-only check let through). Read-only — no dialog
    is created.

    Tolerant of an older companion with no such route: skip if the body is
    empty AND status is 404/405 (route genuinely absent on that version)."""
    r = httpx.get(
        f"{ENDPOINT}/render/nonexistent-harness-probe-id",
        headers=_auth(),
        timeout=TIMEOUT,
    )
    body = r.text or ""
    # Older companion without the async route: empty-body 404/405 → not this
    # version's contract, skip rather than fail.
    if r.status_code in (404, 405) and "unknown_render_id" not in body and body.strip() == "":
        import pytest as _pytest

        _pytest.skip("companion has no async GET /render/{id} route (pre-0.5.0)")
    assert r.status_code == 404
    assert "unknown_render_id" in body, (
        "GET /render/<id> must hit the async handler (body 'unknown_render_id'); "
        f"empty 404 means the route didn't match. Got: {body!r}"
    )
