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

def patch_body(uvx_path: str | None = None, version: str = "9.9.9") -> str:
    """The patch script as `setup.rs` renders it, for a given probe result.

    `uvx_path=None` is what the startup resync and the Resync button pass
    when no absolute path is known for the host — the case that used to
    downgrade a working pin (#184).
    """
    cmd_lit = json.dumps(uvx_path if uvx_path else "uvx")
    known = "True" if uvx_path else "False"
    pkg_lit = json.dumps(f"aiui-mcp=={version}")
    return f"""
servers = data.get("mcpServers") or {{}}
existing = servers.get("aiui") or {{}}
current_cmd = existing.get("command") if isinstance(existing, dict) else None

want_cmd = {cmd_lit}
if not {known}:
    if isinstance(current_cmd, str) and current_cmd.startswith("/") \\
            and current_cmd.rstrip("/").endswith("/uvx"):
        want_cmd = current_cmd

if (current_cmd == want_cmd and existing.get("args") == [{pkg_lit}]):
    print("ok:current")
    raise SystemExit(0)
backup()
entry = dict(existing) if isinstance(existing, dict) else {{}}
entry["command"] = want_cmd
entry["args"] = [{pkg_lit}]
servers["aiui"] = entry
data["mcpServers"] = servers
save(data)
print("ok:patched")
"""


PATCH_BODY = patch_body()

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


# --- #184: a path-less resync must not downgrade a working command

ABS_UVX = "/opt/homebrew/bin/uvx"


def _entry(home: Any, command: str, version: str = "9.9.9", **extra: Any) -> None:
    _cfg(home).write_text(
        json.dumps(
            {
                "mcpServers": {
                    "aiui": {"command": command, "args": [f"aiui-mcp=={version}"], **extra},
                    "other": {"command": "keep-me"},
                },
                "oauthAccount": {"id": "abc"},
            },
            indent=2,
        )
    )


def _command(home: Any) -> Any:
    return json.loads(_cfg(home).read_text())["mcpServers"]["aiui"]["command"]


def test_a_pathless_resync_keeps_the_absolute_uvx_path(tmp_path: Any) -> None:
    """The #184 regression itself.

    `add_remote` probes for an absolute uvx path because the bare name
    depends on Claude Code's PATH at spawn time. The startup resync — which
    runs for every remote on every launch — passed no path and rewrote that
    pin down to `"uvx"`, reporting success while breaking the host.
    """
    _entry(tmp_path, ABS_UVX)
    r = _run(patch_body(None), tmp_path)
    assert r.stdout.strip() == "ok:current", r.stderr
    assert _command(tmp_path) == ABS_UVX, "the discovered path must survive"


def test_a_pathless_resync_still_enforces_the_version_pin(tmp_path: Any) -> None:
    """Keeping the command must not cost the pin — that pin is what stops
    uvx caching a stale aiui-mcp indefinitely."""
    _entry(tmp_path, ABS_UVX, version="0.1.0")
    r = _run(patch_body(None), tmp_path)
    assert r.stdout.strip() == "ok:patched", r.stderr
    d = json.loads(_cfg(tmp_path).read_text())["mcpServers"]["aiui"]
    assert d["args"] == ["aiui-mcp==9.9.9"], "re-pinned"
    assert d["command"] == ABS_UVX, "…without downgrading the command"


def test_a_discovered_path_always_wins(tmp_path: Any) -> None:
    _entry(tmp_path, "uvx")
    _run(patch_body(ABS_UVX), tmp_path)
    assert _command(tmp_path) == ABS_UVX


@pytest.mark.parametrize(
    ("existing", "why"),
    [
        ("uvx", "a bare name is not something to preserve"),
        ("/usr/bin/python3", "an unrelated absolute command is not a uvx"),
        ("/opt/uvx-wrapper", "does not end in /uvx"),
    ],
)
def test_only_a_resolvable_uvx_is_preserved(tmp_path: Any, existing: str, why: str) -> None:
    _entry(tmp_path, existing, version="0.1.0")
    _run(patch_body(None), tmp_path)
    assert _command(tmp_path) == "uvx", why


def test_a_fresh_host_still_gets_the_bare_name(tmp_path: Any) -> None:
    _cfg(tmp_path).write_text(json.dumps({"mcpServers": {}}))
    _run(patch_body(None), tmp_path)
    assert _command(tmp_path) == "uvx"


def test_the_rest_of_the_config_is_untouched_either_way(tmp_path: Any) -> None:
    _entry(tmp_path, ABS_UVX, version="0.1.0", env={"K": "v"})
    _run(patch_body(None), tmp_path)
    d = json.loads(_cfg(tmp_path).read_text())
    assert d["mcpServers"]["aiui"]["env"] == {"K": "v"}
    assert d["mcpServers"]["other"]["command"] == "keep-me"
    assert d["oauthAccount"] == {"id": "abc"}
