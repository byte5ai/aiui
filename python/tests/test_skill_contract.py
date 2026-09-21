"""Self-test for `scripts/check-skill-drift.sh` (#205).

That script is the only automated check on the agent-facing contract: what
the two skill.md copies document versus what the two bridges actually
accept. Before #205 it compared backtick-quoted tokens found in Markdown
*headers* only — 23 tokens — so a field kind with no header of its own was
invisible to it. `tree` shipped fully implemented, validated by
`KNOWN_FIELD_KINDS` and advertised in both tool schemas while appearing in
neither catalog, and CI stayed green the whole time: the section was missing
from *both* copies, so the symmetric comparison had nothing to report.

A guard that silently stopped guarding is worse than none, so the widened
assertions are exercised against mutated fixture copies of the real
sources. Each test copies the five files the guard reads into a temp
directory, breaks exactly one of them, and asserts the guard fails and names
the thing that broke.

Skipped when the Rust sources are absent (an installed-wheel run) so the
published package stays testable.

Still uncovered, recorded as a follow-up: nothing here compares the two
bridges' JSON input *schemas* — shared parameter names and their defaults.
A default that diverges between the Rust and the Python bridge passes every
assertion below (`test_shared_parameter_defaults_agree` would be the place
for it) and is the remaining class of silent remote-vs-local drift.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
GUARD = REPO / "scripts" / "check-skill-drift.sh"
CANON = Path("docs/skill.md")
COPY = Path("python/src/aiui_mcp/skill.md")
HTTP_RS = Path("companion/src-tauri/src/http.rs")
MCP_RS = Path("companion/src-tauri/src/mcp.rs")
SERVER_PY = Path("python/src/aiui_mcp/server.py")

GUARD_INPUTS = (CANON, COPY, HTTP_RS, MCP_RS, SERVER_PY)

pytestmark = pytest.mark.skipif(
    os.name == "nt" or not all((REPO / p).is_file() for p in (GUARD, *GUARD_INPUTS)),
    reason="bash drift-guard — validated on the Linux leg and the skill-drift "
    "CI job; also needs a repo checkout (the guard reads the Rust/Python sources)",
)


def _fixture_repo(tmp_path: Path) -> Path:
    """A minimal copy of the repo holding just what the guard reads."""
    root = tmp_path / "repo"
    for rel in (Path("scripts/check-skill-drift.sh"), *GUARD_INPUTS):
        dest = root / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPO / rel, dest)
    return root


def _run_guard(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(root / "scripts" / "check-skill-drift.sh")],
        capture_output=True,
        text=True,
    )


def _patch(root: Path, rel: Path, old: str, new: str) -> None:
    path = root / rel
    text = path.read_text(encoding="utf-8")
    assert text.count(old) == 1, f"expected exactly one {old!r} in {rel}"
    path.write_text(text.replace(old, new), encoding="utf-8")


def _drop_section(root: Path, rel: Path, header: str) -> None:
    """Remove a level-2 section (header through the next level-2 header)."""
    path = root / rel
    text = path.read_text(encoding="utf-8")
    start = text.index(header)
    end = text.index("\n## ", start + len(header)) + 1
    path.write_text(text[:start] + text[end:], encoding="utf-8")


def _undocument(root: Path, rel: Path, header: str, token: str) -> None:
    """Leave no trace of a field kind in one copy: section and any mention."""
    _drop_section(root, rel, header)
    path = root / rel
    kept = [
        line
        for line in path.read_text(encoding="utf-8").splitlines(keepends=True)
        if f"`{token}`" not in line
    ]
    path.write_text("".join(kept), encoding="utf-8")


def test_guard_passes_on_the_repo() -> None:
    """The contract holds as committed — `tree` included."""
    proc = _run_guard(REPO)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "skill-drift: OK" in proc.stdout


def test_missing_field_kind_fails_guard(tmp_path: Path) -> None:
    """The #205 hole itself: a kind the validator accepts, documented nowhere.

    Undocumented in *both* copies, which is the state that shipped: the
    section comparison has nothing to report and only the field-kind check
    can see it.
    """
    root = _fixture_repo(tmp_path)
    header = "## Hierarchical picker: `tree`"
    _undocument(root, CANON, header, "tree")
    _undocument(root, COPY, header, "tree")

    proc = _run_guard(root)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    assert "not documented in both skill.md copies" in out
    assert "tree" in out
    # The point of the widened guard: the old header comparison stays quiet
    # on a symmetric hole.
    assert "field/tool sections in" not in out


def test_one_sided_section_removal_fails_guard(tmp_path: Path) -> None:
    """Dropping the section from one copy only: the section check catches it."""
    root = _fixture_repo(tmp_path)
    _drop_section(root, COPY, "## Hierarchical picker: `tree`")

    proc = _run_guard(root)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    assert "field/tool sections in docs/skill.md but NOT in" in out
    assert "tree" in out


def test_unknown_field_kind_fails_guard(tmp_path: Path) -> None:
    """The kind list is parsed out of the Rust source, not hardcoded here."""
    root = _fixture_repo(tmp_path)
    _patch(root, HTTP_RS, '"list", "table", "tree",', '"list", "table", "tree", "foo",')

    proc = _run_guard(root)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    assert "not documented in both skill.md copies" in out
    assert "foo" in out


def test_tool_name_mismatch_fails_guard(tmp_path: Path) -> None:
    """A tool renamed on one bridge only is a capability the other host loses."""
    root = _fixture_repo(tmp_path)
    _patch(root, MCP_RS, '"name": "compare"', '"name": "compare2"')

    proc = _run_guard(root)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    assert "NOT by the Python bridge" in out
    assert "compare2" in out
    assert "NOT by the Rust bridge" in out
