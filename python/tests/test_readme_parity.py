"""Tests that the PyPI landing page and the bridge's own prose keep up
with the code (#204).

Two drifts had accumulated: `README.md` — the `readme` of this package, so
the page rendered on pypi.org — documented 4 of 10 tools and 1 of 7 prompts
under an `aiui.` prefix no MCP client uses, and every diagnostic in
`server.py` named a Mac although aiui ships on Windows since v0.10.1. Both
are asserted against the source of truth rather than a hand-kept copy: the
live `FastMCP` instance for the tool/prompt sets, the module's own AST for
the strings.

Note the deliberate asymmetry with the Rust bridge: this side must NOT
branch on `sys.platform`, because it runs on the *remote* host — a Linux
dev box would print Linux instructions for a Windows companion. Neutral
wording is the only correct answer here.
"""

from __future__ import annotations

import ast
import asyncio
import re
from pathlib import Path

import aiui_mcp.server as server

README = Path(__file__).resolve().parents[1] / "README.md"

# Lines of a string constant that may name a platform, with the reason.
# Keep this list short — each entry is a promise that the sentence is about
# a genuinely OS-specific behaviour, not stale Mac-only wording.
PLATFORM_ALLOWLIST = (
    # `notify`: the permission prompt really is a macOS-only OS behaviour.
    "notification permission on macOS",
    # `_default_token_path`: documents the real per-OS token locations, the
    # very thing #204 asks the docs to state honestly.
    "Linux / macOS: `~/.config/aiui/token`",
)

PLATFORM_RE = re.compile(r"\bMac\b|macOS|/Applications")


def _registered() -> tuple[set[str], set[str]]:
    async def collect() -> tuple[set[str], set[str]]:
        tools = {t.name for t in await server.mcp.list_tools()}
        prompts = {p.name for p in await server.mcp.list_prompts()}
        return tools, prompts

    return asyncio.run(collect())


def test_readme_documents_every_tool_and_prompt() -> None:
    tools, prompts = _registered()

    # Pinned so a *new* tool or prompt fails here too, not just a renamed
    # one — the README grew stale precisely by staying silent as the
    # surface grew from three tools to ten.
    assert len(tools) == 10, f"tool count changed: {sorted(tools)}"
    assert len(prompts) == 7, f"prompt count changed: {sorted(prompts)}"

    readme = README.read_text(encoding="utf-8")
    for name in sorted(tools):
        assert f"`{name}`" in readme, f"tool {name} is missing from python/README.md"
    for name in sorted(prompts):
        assert f"`/aiui:{name}`" in readme, f"prompt /aiui:{name} is missing from python/README.md"

    # The dotted form was never a real MCP tool name; readers copied it.
    assert "aiui.confirm" not in readme, "README still uses the bogus `aiui.` tool prefix"


def test_user_facing_strings_are_platform_neutral() -> None:
    source = Path(server.__file__).read_text(encoding="utf-8")
    tree = ast.parse(source)

    offenders: list[str] = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Constant) or not isinstance(node.value, str):
            continue
        for line in node.value.splitlines():
            if not PLATFORM_RE.search(line):
                continue
            if any(snippet in line for snippet in PLATFORM_ALLOWLIST):
                continue
            offenders.append(f"line {node.lineno}: {line.strip()}")

    assert not offenders, "platform-specific wording reaches the user:\n" + "\n".join(offenders)


SKILL_MD = Path(server.__file__).resolve().parent / "skill.md"
CANON_SKILL_MD = Path(__file__).resolve().parents[2] / "docs" / "skill.md"

# macOS-only vocabulary that must not reach an agent on a Windows companion.
SKILL_PLATFORM_RE = re.compile(r"\bMacs?\b|macOS|\bFinder\b|/Applications|aiui\.app\b")

# Lines of the skill catalog that may name a platform. Only the notification
# permission prompt is genuinely macOS-only behaviour.
SKILL_PLATFORM_ALLOWLIST = ("On macOS the first call triggers the one-time",)


def _skill_platform_offenders(path: Path) -> list[str]:
    offenders = []
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if SKILL_PLATFORM_RE.search(line) and not any(a in line for a in SKILL_PLATFORM_ALLOWLIST):
            offenders.append(f"{path.name}:{n}: {line.strip()}")
    return offenders


def test_skill_catalog_is_platform_neutral() -> None:
    """F-14: #252 put "Retry once the user's Mac is back." into both skill
    catalogs — the text `/aiui:teach` hands every agent — so a Windows user's
    agent told them to wait for a Mac. Nothing scanned the catalog; only
    `server.py`'s own strings were checked."""
    offenders = _skill_platform_offenders(SKILL_MD)
    if CANON_SKILL_MD.is_file():  # repo checkout: the canonical copy too
        offenders += _skill_platform_offenders(CANON_SKILL_MD)
    assert not offenders, "macOS-only wording in the skill catalog:\n" + "\n".join(offenders)
