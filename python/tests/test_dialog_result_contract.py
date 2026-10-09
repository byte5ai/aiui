"""The dialog-result contract, held against one shared fixture (E-02, #273).

`schemas/dialog-results.json` is the single statement of what a cancelled
`ask`/`form`/`gallery`/`compare` returns. Both bridges' tests read it: before
it existed each bridge pinned its own idea of the shape — Python the documented
falsy keys, Rust a bare `{cancelled: true}` — so an agent reading
`result["values"]` after a cancel worked on one side and hit a KeyError on the
other. The same fixture is checked against the "Reading the result" table in
both skill.md copies, so the docs cannot drift from it either.

#273: the tool descriptions are what the model reads first. They must say
what `reason` means — otherwise a dialog that ended with nobody answering
(`ttl_expired`, `abandoned`, …) reads exactly like the user declining.

Skipped where the repo files are absent (an installed-wheel run).
"""

from __future__ import annotations

import asyncio
import json
import re
from pathlib import Path
from typing import Any

import pytest

import aiui_mcp.server as server
from aiui_mcp.server import _cancel_defaults, _format_result

REPO = Path(__file__).resolve().parents[2]
FIXTURE = REPO / "schemas" / "dialog-results.json"
SKILL_COPIES = (REPO / "docs" / "skill.md", REPO / "python" / "src" / "aiui_mcp" / "skill.md")

DIALOG_TOOLS = ("confirm", "ask", "form", "gallery", "compare")
CANCEL_REASONS = ("ttl_expired", "evicted", "channel_dropped", "host_exiting", "abandoned")

needs_fixture = pytest.mark.skipif(not FIXTURE.is_file(), reason="needs a repo checkout")


def _cancel_defaults_fixture() -> dict[str, dict[str, Any]]:
    return json.loads(FIXTURE.read_text(encoding="utf-8"))["cancel_defaults"]


@needs_fixture
def test_fixture_covers_the_shared_formatter_kinds() -> None:
    """`confirm` has its own formatter on the Rust side and is deliberately
    not in the fixture; every other dialog kind is."""
    assert set(_cancel_defaults_fixture()) == {"ask", "form", "gallery", "compare"}


@needs_fixture
@pytest.mark.parametrize("kind", ["ask", "form", "gallery", "compare"])
def test_cancel_shape_matches_the_shared_fixture(kind: str) -> None:
    defaults = _cancel_defaults_fixture()[kind]
    assert _cancel_defaults(kind) == defaults
    plain = {"id": "d1", "cancelled": True, "result": None}
    assert _format_result(plain, kind) == {"cancelled": True, **defaults}
    with_reason = {**plain, "reason": "abandoned"}
    assert _format_result(with_reason, kind) == {
        "cancelled": True,
        **defaults,
        "reason": "abandoned",
    }


def test_confirm_cancel_keeps_confirmed_false() -> None:
    """Not in the fixture (separate Rust formatter), documented in skill.md."""
    assert _format_result({"cancelled": True, "result": None}, "confirm") == {
        "cancelled": True,
        "confirmed": False,
    }


def _js(value: Any) -> str:
    """The fixture value as the skill.md table writes it: `[]`, `{}`."""
    return json.dumps(value, separators=(", ", ": "))


@needs_fixture
@pytest.mark.parametrize("copy", SKILL_COPIES, ids=lambda p: str(p.relative_to(REPO)))
def test_skill_cancel_table_matches_the_fixture(copy: Path) -> None:
    if not copy.is_file():
        pytest.skip("skill copy absent")
    text = copy.read_text(encoding="utf-8")
    for kind, defaults in _cancel_defaults_fixture().items():
        row = next(
            (line for line in text.splitlines() if line.startswith(f"| `{kind}` |")),
            None,
        )
        assert row is not None, f"{copy.name}: no result-table row for `{kind}`"
        expected = "{cancelled: true" + "".join(f", {k}: {_js(v)}" for k, v in defaults.items())
        assert f"`{expected}}}`" in row, f"{copy.name}: `{kind}` cancel cell drifted: {row}"


@pytest.mark.parametrize("copy", SKILL_COPIES, ids=lambda p: p.parent.name)
def test_skill_reason_table_lists_every_reason(copy: Path) -> None:
    """`abandoned` is new (the companion's reaper cancels with it); an agent
    that never read about it treats it as an unknown reason at best."""
    if not copy.is_file():
        pytest.skip("skill copy absent")
    text = copy.read_text(encoding="utf-8")
    for reason in CANCEL_REASONS:
        assert re.search(rf"^\| `{reason}` \|", text, re.M), f"{copy.name}: `{reason}` row missing"


def _tool_descriptions() -> dict[str, str]:
    async def collect() -> dict[str, str]:
        return {t.name: t.description or "" for t in await server.mcp.list_tools()}

    return asyncio.run(collect())


@pytest.mark.parametrize("tool", DIALOG_TOOLS)
def test_dialog_tool_description_explains_reason_and_media_warnings(tool: str) -> None:
    """#273: without this, a `ttl_expired` cancel reads to the model exactly
    like the user pressing No, and the model acts on a decision nobody made."""
    description = " ".join(_tool_descriptions()[tool].split())
    for reason in CANCEL_REASONS:
        assert f"`{reason}`" in description, f"{tool}: reason `{reason}` not described"
    assert "only a reason-less cancel is a user decision" in description, tool
    assert "`media_warnings`" in description, tool


@pytest.mark.parametrize("copy", SKILL_COPIES, ids=lambda p: p.parent.name)
def test_skill_description_names_every_dialog_tool(copy: Path) -> None:
    """#274: the Python copy's frontmatter omitted `compare` and `gallery`, so
    sessions on that bridge got weaker routing from the very first line."""
    if not copy.is_file():
        pytest.skip("skill copy absent")
    frontmatter = copy.read_text(encoding="utf-8").split("---", 2)[1]
    description = next(line for line in frontmatter.splitlines() if line.startswith("description:"))
    for tool in (*DIALOG_TOOLS, "notify"):
        assert f"`{tool}`" in description, f"{copy.name}: `{tool}` missing from description"


def test_form_description_shows_the_newline_escape_literally() -> None:
    """The wireframe line tells the model to write `\\n` inside `content`.
    In a non-raw docstring the two characters became a real line break, so
    the model read "escape `" + newline + "`" — an instruction with its
    subject missing (the Rust text had the same defect, #274)."""
    description = _tool_descriptions()["form"]
    assert "escape `\\n`" in description
