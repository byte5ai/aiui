"""The two bridges' tool schemas must not drift apart (#211).

aiui ships the same tool surface twice: hand-written JSON Schema in the Rust
bridge (`companion/src-tauri/src/mcp.rs::tools_list`) and FastMCP-generated
schemas here. Nothing compared them, and they had already diverged —
`ask.allow_other` defaults differently on the two sides, which an agent only
notices as "the free-text option is there on one host and not the other".

Both bridges now assert against `schemas/tools-schema.json`; the Rust half
lives in `mcp.rs`'s test module. Structure only — names, required[],
argument names, types and scalar defaults — never the descriptions, which are
prose that changes on purpose.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

from aiui_mcp.server import mcp

FIXTURE = Path(__file__).resolve().parents[2] / "schemas" / "tools-schema.json"

BRIDGE = "python"


def _fixture() -> dict[str, Any]:
    return json.loads(FIXTURE.read_text(encoding="utf-8"))["tools"]


def _listed_schemas() -> dict[str, dict[str, Any]]:
    tools = asyncio.run(mcp.list_tools())
    return {t.name: t.inputSchema for t in tools}


def _prop_type(prop: dict[str, Any]) -> str:
    """Mirror of the normalisation documented in the fixture."""
    t = prop.get("type")
    if isinstance(t, str):
        return "number" if t == "integer" else t
    if "anyOf" in prop:
        types = sorted(
            {"number" if x.get("type") == "integer" else x.get("type") for x in prop["anyOf"]}
            - {"null"}
        )
        if len(types) == 1:
            return str(types[0])
        return "|".join(str(x) for x in types)
    return "any"


def _actual_default(prop: dict[str, Any]) -> Any:
    """`default: null` is FastMCP's way of saying "no default"."""
    d = prop.get("default")
    if d is None or not isinstance(d, (bool, int, float, str)):
        return None
    return d


def _expected_default(entry: dict[str, Any]) -> Any:
    """An object default is a known divergence — take this bridge's side."""
    d = entry.get("default")
    if isinstance(d, dict):
        assert BRIDGE in d, f"a divergence entry names both bridges: {d}"
        return d[BRIDGE]
    return d


def test_tool_set_matches_the_shared_fixture() -> None:
    assert sorted(_listed_schemas()) == sorted(_fixture()), (
        "tool set drifted from schemas/tools-schema.json"
    )


def test_required_arguments_match_the_shared_fixture() -> None:
    actual = _listed_schemas()
    for name, entry in _fixture().items():
        got = sorted(actual[name].get("required") or [])
        assert got == sorted(entry["required"]), f"{name}: required[] drifted"


def test_argument_names_and_types_match_the_shared_fixture() -> None:
    actual = _listed_schemas()
    for name, entry in _fixture().items():
        props = actual[name].get("properties") or {}
        assert sorted(props) == sorted(entry["properties"]), f"{name}: argument set drifted"
        for prop, want in entry["properties"].items():
            assert _prop_type(props[prop]) == want["type"], f"{name}.{prop}: type drifted"


def test_argument_defaults_match_the_shared_fixture() -> None:
    # The assertion that would have caught `ask.allow_other`: a default
    # changed on one bridge only now fails on that bridge.
    actual = _listed_schemas()
    for name, entry in _fixture().items():
        props = actual[name].get("properties") or {}
        for prop, want in entry["properties"].items():
            assert _actual_default(props[prop]) == _expected_default(want), (
                f"{name}.{prop}: default drifted"
            )


def test_allow_other_default_is_aligned_across_bridges() -> None:
    # #211 first recorded `ask.allow_other` as a two-bridge divergence
    # (rust=false, python=true); #203 aligned them on false — "false is the
    # settled value". The fixture now records a single default rather than a
    # `{rust, python}` split, and `test_argument_defaults_match_the_shared_
    # fixture` holds each bridge to it, so a future one-sided regression fails
    # loudly there.
    entry = _fixture()["ask"]["properties"]["allow_other"]["default"]
    assert entry is False, f"expected the aligned scalar default, got {entry!r}"
