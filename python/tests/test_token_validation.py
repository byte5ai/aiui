"""Bridge-side token validation (#185).

A well-formed aiui token is 64 hex chars. Anything else — empty, truncated
by a failed copy, mangled by an editor — must fail loudly here rather than
being sent as a bare or partial `Bearer` that the companion rejects with an
opaque 401.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from aiui_mcp import server

GOOD = "de1e7e57" * 8


def _point_at(monkeypatch: pytest.MonkeyPatch, path: Path) -> None:
    monkeypatch.setattr("aiui_mcp.server.TOKEN_PATH", path)


def test_accepts_a_well_formed_token(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    p = tmp_path / "token"
    p.write_text(GOOD + "\n")  # trailing newline is normal and must be tolerated
    _point_at(monkeypatch, p)
    assert server._token() == GOOD


@pytest.mark.parametrize(
    ("content", "why"),
    [
        ("", "empty file"),
        ("   \n", "whitespace only"),
        (GOOD[:-1], "truncated by one char"),
        (GOOD + "a", "one char too long"),
        ("z" * 64, "right length, not hex"),
        ("dummy-token-for-tests", "a placeholder someone pasted"),
    ],
)
def test_rejects_malformed_tokens(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any, content: str, why: str
) -> None:
    p = tmp_path / "token"
    p.write_text(content)
    _point_at(monkeypatch, p)
    with pytest.raises(RuntimeError) as exc:
        server._token()
    msg = str(exc.value)
    assert "malformed" in msg, why
    assert str(p) in msg, "the message names the file so the user can fix it"


def test_missing_token_still_explains_how_to_get_one(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _point_at(monkeypatch, tmp_path / "does-not-exist")
    with pytest.raises(RuntimeError) as exc:
        server._token()
    assert "not found" in str(exc.value)
