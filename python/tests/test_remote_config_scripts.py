"""The remote `~/.claude.json` scripts, executed for real (#182).

`setup.rs` ships two Python snippets that run on a registered remote host
over ssh: one pins the `aiui-mcp` version, one removes the entry on
deregistration. They are Rust string constants, so nothing in the Rust
test suite can execute them — a syntax error or a destructive edit would
only surface on a user's remote host.

This test extracts the shared preamble straight out of `setup.rs` and runs
both scripts against a temporary HOME. It is a contract test across the
language boundary: if someone edits the preamble, this fails here rather
than on someone's dev box.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

SETUP_RS = (
    Path(__file__).resolve().parents[2]
    / "companion"
    / "src-tauri"
    / "src"
    / "setup.rs"
)

PATCH_BODY = """
servers = data.get("mcpServers") or {}
existing = servers.get("aiui") or {}
if (existing.get("command") == "uvx"
        and existing.get("args") == ["aiui-mcp==9.9.9"]):
    print("ok:current")
    raise SystemExit(0)
backup()
entry = dict(existing) if isinstance(existing, dict) else {}
entry["command"] = "uvx"
entry["args"] = ["aiui-mcp==9.9.9"]
servers["aiui"] = entry
data["mcpServers"] = servers
save(data)
print("ok:patched")
"""

REMOVE_BODY = """
if not p.exists():
    print("ok")
else:
    servers = data.get("mcpServers") or {}
    if "aiui" in servers:
        backup()
        servers.pop("aiui", None)
        data["mcpServers"] = servers
        save(data)
    print("ok")
"""


def _preamble() -> str:
    """The `REMOTE_JSON_PREAMBLE` constant, read from the Rust source."""
    if not SETUP_RS.exists():  # installed wheel, no repo checkout
        pytest.skip("setup.rs not available outside the repo")
    m = re.search(
        r'const REMOTE_JSON_PREAMBLE: &str = r#"(.*?)"#;', SETUP_RS.read_text(), re.S
    )
    assert m, "REMOTE_JSON_PREAMBLE not found — did the constant get renamed?"
    return m.group(1)


def _run(body: str, home: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-c", _preamble() + body],
        capture_output=True,
        text=True,
        env={**os.environ, "HOME": str(home)},
    )


def _cfg(home: Path) -> Path:
    return home / ".claude.json"


HEALTHY = {
    "mcpServers": {
        "other": {"command": "keep-me"},
        "aiui": {"command": "uvx", "args": ["aiui-mcp==0.1.0"], "env": {"D": "1"}},
    },
    "oauthAccount": {"id": "abc"},
    "projects": {"/some/project": {"history": ["x"]}},
}


@pytest.mark.parametrize("body", [PATCH_BODY, REMOVE_BODY], ids=["patch", "remove"])
def test_malformed_config_is_left_byte_identical(body: str, tmp_path: Any) -> None:
    """The #182 headline: one trailing comma used to cost the user every MCP
    server, every project entry and the OAuth block."""
    original = '{"mcpServers": {"other": {"command": "x"},}}'
    _cfg(tmp_path).write_text(original)
    r = _run(body, tmp_path)
    assert r.stdout.strip() == "err:malformed", r.stderr
    assert r.returncode == 2
    assert _cfg(tmp_path).read_text() == original, "file must not be touched"


@pytest.mark.parametrize("body", [PATCH_BODY, REMOVE_BODY], ids=["patch", "remove"])
def test_non_object_json_also_bails(body: str, tmp_path: Any) -> None:
    _cfg(tmp_path).write_text("[1, 2, 3]")
    r = _run(body, tmp_path)
    assert r.stdout.strip() == "err:malformed", r.stderr
    assert _cfg(tmp_path).read_text() == "[1, 2, 3]"


def test_patch_preserves_everything_it_does_not_own(tmp_path: Any) -> None:
    _cfg(tmp_path).write_text(json.dumps(HEALTHY, indent=2))
    r = _run(PATCH_BODY, tmp_path)
    assert r.stdout.strip() == "ok:patched", r.stderr
    d = json.loads(_cfg(tmp_path).read_text())
    assert d["mcpServers"]["aiui"]["args"] == ["aiui-mcp==9.9.9"], "pin applied"
    assert d["mcpServers"]["aiui"]["env"] == {"D": "1"}, "foreign key on the entry"
    assert d["mcpServers"]["other"]["command"] == "keep-me", "other MCP server"
    assert d["oauthAccount"] == {"id": "abc"}, "credentials"
    assert d["projects"] == HEALTHY["projects"], "project history"
    assert len(list(tmp_path.glob(".claude.json.bak.*"))) == 1, "backup next to the file"


def test_patch_is_idempotent_and_does_not_pile_up_backups(tmp_path: Any) -> None:
    _cfg(tmp_path).write_text(json.dumps(HEALTHY, indent=2))
    _run(PATCH_BODY, tmp_path)
    r = _run(PATCH_BODY, tmp_path)
    assert r.stdout.strip() == "ok:current", r.stderr
    assert len(list(tmp_path.glob(".claude.json.bak.*"))) == 1, "no backup on a no-op"


def test_backups_are_pruned(tmp_path: Any) -> None:
    for i in range(9):
        d = json.loads(json.dumps(HEALTHY))
        d["mcpServers"]["aiui"]["args"] = [f"aiui-mcp==0.{i}.0"]
        _cfg(tmp_path).write_text(json.dumps(d, indent=2))
        _run(PATCH_BODY, tmp_path)
    assert len(list(tmp_path.glob(".claude.json.bak.*"))) <= 5


def test_remove_takes_only_the_aiui_entry(tmp_path: Any) -> None:
    _cfg(tmp_path).write_text(json.dumps(HEALTHY, indent=2))
    r = _run(REMOVE_BODY, tmp_path)
    assert r.stdout.strip() == "ok", r.stderr
    d = json.loads(_cfg(tmp_path).read_text())
    assert "aiui" not in d["mcpServers"]
    assert d["mcpServers"]["other"]["command"] == "keep-me"
    assert d["oauthAccount"] == {"id": "abc"}
    assert len(list(tmp_path.glob(".claude.json.bak.*"))) == 1, "remove backs up too"


def test_absent_config_is_handled_by_both(tmp_path: Any) -> None:
    r = _run(REMOVE_BODY, tmp_path)
    assert r.stdout.strip() == "ok", r.stderr
    r = _run(PATCH_BODY, tmp_path)
    assert r.stdout.strip() == "ok:patched", r.stderr
    assert _cfg(tmp_path).exists()


def test_no_temp_file_is_left_behind(tmp_path: Any) -> None:
    _cfg(tmp_path).write_text(json.dumps(HEALTHY, indent=2))
    _run(PATCH_BODY, tmp_path)
    _run(REMOVE_BODY, tmp_path)
    assert not list(tmp_path.glob("*.aiui-tmp"))
