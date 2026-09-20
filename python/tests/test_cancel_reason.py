"""The cancel reason must reach the agent (#180).

The companion sets `host_exiting`, `ttl_expired`, `evicted` and
`channel_dropped`, but the bridge flattened every one into a bare
`{"cancelled": true}` — indistinguishable from the user pressing Escape.
Forwarding is generic, so a reason added later needs no bridge change. An agent that cannot tell "the user declined" from
"the companion was shutting down" retries the wrong thing.

I6: this must behave identically to the Rust bridge's
`format_dialog_result`, which has the mirror tests.
"""

from __future__ import annotations

import pytest

from aiui_mcp.server import _format_result


@pytest.mark.parametrize(
    "reason",
    ["host_exiting", "ttl_expired", "evicted", "channel_dropped", "future_reason"],
)
def test_cancel_reason_is_forwarded(reason: str) -> None:
    out = _format_result({"id": "d1", "cancelled": True, "result": None, "reason": reason})
    assert out == {"cancelled": True, "reason": reason}


def test_plain_user_cancel_carries_no_reason() -> None:
    """Escape has no reason attached; inventing one is worse than omitting it."""
    assert _format_result({"id": "d1", "cancelled": True, "result": None}) == {"cancelled": True}


def test_empty_or_non_string_reason_is_dropped() -> None:
    assert _format_result({"cancelled": True, "reason": ""}) == {"cancelled": True}
    assert _format_result({"cancelled": True, "reason": None}) == {"cancelled": True}
    assert _format_result({"cancelled": True, "reason": 42}) == {"cancelled": True}


def test_submitted_dialog_keeps_values_and_gains_no_reason() -> None:
    """The happy path must not regress, and a stale reason on a successful
    submit would be actively misleading."""
    out = _format_result(
        {"cancelled": False, "result": {"values": {"name": "Ada"}}, "reason": "host_exiting"}
    )
    assert out == {"cancelled": False, "values": {"name": "Ada"}}


def test_media_warnings_reach_the_agent() -> None:
    """#194: a clip that never made it into the media cache left the user with
    a broken player and the agent with no signal at all. Forwarded on both
    branches — a user may well cancel *because* of the broken player."""
    warnings = ["video not shown — /tmp/clip.mp4: too large: 999 bytes (max 512)"]
    out = _format_result({"cancelled": False, "result": {"values": {}}, "media_warnings": warnings})
    assert out == {"cancelled": False, "values": {}, "media_warnings": warnings}

    out = _format_result({"cancelled": True, "result": None, "media_warnings": warnings})
    assert out == {"cancelled": True, "media_warnings": warnings}


def test_clean_render_has_no_media_warnings_key() -> None:
    """An always-present empty list trains agents to ignore the key."""
    out = _format_result({"cancelled": False, "result": {"values": {}}})
    assert "media_warnings" not in out
    out = _format_result({"cancelled": False, "result": {}, "media_warnings": []})
    assert "media_warnings" not in out
