"""The bridge's poll-outage budget must fit inside the companion's patience (E-01).

The companion reaps an async-render slot nobody has polled for
`SLOT_ABANDONED_AFTER` and closes its dialog (`companion/src-tauri/src/http.rs`).
A bridge that is still retrying after that comes back to a 404 and a dialog
whose input is gone. So the longest stretch between two polls that reach the
companion — the whole outage budget, plus one backoff, plus one timed-out GET —
has to stay below that threshold:

    POLL_OUTAGE_BUDGET_S + ASYNC_POLL_MAX_BACKOFF_S + ASYNC_POLL_TIMEOUT_S
        < SLOT_ABANDONED_AFTER

The two sides live in two languages, so this test reads the Rust constant out of
the source — the same way `scripts/check-skill-drift.sh` reads
`KNOWN_FIELD_KINDS` — instead of trusting a copy. Skipped when the Rust source
is absent (an installed-wheel run).
"""

from __future__ import annotations

import asyncio
import re
from pathlib import Path
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server

HTTP_RS = Path(__file__).resolve().parents[2] / "companion" / "src-tauri" / "src" / "http.rs"

# `const SLOT_ABANDONED_AFTER: Duration = Duration::from_secs(300);` — also
# tolerates `std::time::Duration` and a `5 * 60` product.
_CONST_RE = re.compile(
    r"const\s+SLOT_ABANDONED_AFTER\s*:\s*(?:std::time::)?Duration\s*=\s*"
    r"(?:std::time::)?Duration::from_secs\(\s*(\d+)\s*(?:\*\s*(\d+)\s*)?\)\s*;"
)


def slot_abandoned_after_secs(source: str) -> int:
    m = _CONST_RE.search(source)
    if m is None:
        raise AssertionError(
            "could not parse SLOT_ABANDONED_AFTER out of companion/src-tauri/src/http.rs "
            "— the const moved or changed shape; update _CONST_RE in this test"
        )
    secs = int(m.group(1))
    if m.group(2):
        secs *= int(m.group(2))
    return secs


def test_parser_accepts_the_known_shapes() -> None:
    old = "const SLOT_ABANDONED_AFTER: Duration = Duration::from_secs(90);"
    new = "const SLOT_ABANDONED_AFTER: Duration = Duration::from_secs(300);"
    product = "const SLOT_ABANDONED_AFTER: Duration = Duration::from_secs(5 * 60);"
    visible = "pub(crate) const SLOT_ABANDONED_AFTER: Duration = Duration::from_secs(300);"
    assert slot_abandoned_after_secs(old) == 90
    assert slot_abandoned_after_secs(new) == 300
    assert slot_abandoned_after_secs(product) == 300
    assert slot_abandoned_after_secs(visible) == 300


def _worst_gap() -> float:
    return (
        server.POLL_OUTAGE_BUDGET_S + server.ASYNC_POLL_MAX_BACKOFF_S + server.ASYNC_POLL_TIMEOUT_S
    )


@pytest.mark.skipif(not HTTP_RS.is_file(), reason="needs the companion source (repo checkout)")
def test_outage_budget_fits_inside_the_companion_abandonment_threshold() -> None:
    abandoned_after = slot_abandoned_after_secs(HTTP_RS.read_text(encoding="utf-8"))
    assert _worst_gap() < abandoned_after, (
        f"POLL_OUTAGE_BUDGET_S ({server.POLL_OUTAGE_BUDGET_S:g}) + max backoff "
        f"({server.ASYNC_POLL_MAX_BACKOFF_S:g}) + per-GET timeout "
        f"({server.ASYNC_POLL_TIMEOUT_S:g}) = {_worst_gap():g} s must stay below the "
        f"companion's SLOT_ABANDONED_AFTER ({abandoned_after} s), or a bridge that is "
        f"still retrying returns to a dialog the companion already closed"
    )


@pytest.mark.skipif(not HTTP_RS.is_file(), reason="needs the companion source (repo checkout)")
def test_a_blackholed_link_never_outlasts_the_companion(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """The arithmetic above, exercised: every GET hangs for its full timeout
    (a blackholed link), on a fake clock. Every poll attempt the bridge makes
    must start before the companion would have reaped the slot — otherwise
    the link coming back right then still finds the dialog gone."""
    abandoned_after = slot_abandoned_after_secs(HTTP_RS.read_text(encoding="utf-8"))
    token_file = tmp_path / "token"
    token_file.write_text("de1e7e57" * 8)
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)

    clock = {"t": 0.0}
    attempts: list[float] = []

    def now() -> float:
        return clock["t"]

    async def sleep(delay: float) -> None:
        clock["t"] += delay
        await asyncio.sleep(0)

    monkeypatch.setattr(server, "_now", now)
    monkeypatch.setattr(server, "_sleep", sleep)
    monkeypatch.setattr(server, "ASYNC_POLL_MIN_INTERVAL_S", 0.2)

    class _Pending:
        status_code = 200

        def json(self) -> dict[str, Any]:
            return {"pending": True}

    async def fake_get(self: Any, url: str, **kwargs: Any) -> Any:
        attempts.append(clock["t"])
        if len(attempts) == 1:
            return _Pending()  # the last poll that reached the companion, at t=0
        clock["t"] += server.ASYNC_POLL_TIMEOUT_S
        raise httpx.ReadTimeout("blackholed")

    monkeypatch.setattr(httpx.AsyncClient, "get", fake_get)

    async def run() -> Any:
        async with httpx.AsyncClient() as client:
            return await server._poll_render(client, "x", None)

    with pytest.raises(RuntimeError, match="consecutive poll failures"):
        asyncio.run(run())
    last_reached_companion = attempts[0]
    assert max(attempts) - last_reached_companion < abandoned_after, (
        f"the bridge was still sending polls {max(attempts):g} s after the last one "
        f"the companion saw; it reaps the dialog after {abandoned_after} s"
    )
