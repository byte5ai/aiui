"""Bridge-side file-write tests (#135).

Mirror of the Rust `filewrite` tests. The bridge runs ON the agent's host, so
`target` writes are local file operations here too; secret values are written
and stripped before the result reaches the agent.
"""
from __future__ import annotations

import os
from pathlib import Path

from aiui_mcp.server import (
    _apply_target_writes,
    _collect_target_fields,
    _write_local_target,
)


def test_collect_target_fields_flat_and_tabs() -> None:
    spec = {
        "kind": "form",
        "fields": [{"kind": "secret", "name": "a", "target": {"mode": "create", "path": "/x"}},
                   {"kind": "text", "name": "b"}],
        "tabs": [{"label": "T", "fields": [{"kind": "text", "name": "c", "target": {"mode": "create", "path": "/y"}}]}],
    }
    names = sorted(f["name"] for f in _collect_target_fields(spec))
    assert names == ["a", "c"]


def test_create_writes_and_refuses_clobber(tmp_path: Path) -> None:
    path = tmp_path / "sub" / "key"
    target = {"mode": "create", "path": str(path), "perm": "0600"}
    out = _write_local_target("s3cr3t", target)
    assert out["written"], out
    assert path.read_text() == "s3cr3t"
    assert (path.stat().st_mode & 0o777) == 0o600
    # Refuse clobber without overwrite.
    out2 = _write_local_target("other", target)
    assert not out2["written"] and out2.get("error")
    assert path.read_text() == "s3cr3t"
    # ...unless overwrite.
    out3 = _write_local_target("new", {**target, "overwrite": True})
    assert out3["written"]
    assert path.read_text() == "new"


def test_substitute_replaces_exactly_once(tmp_path: Path) -> None:
    path = tmp_path / "config.yaml"
    path.write_text("token: __PAT__\nother: 1\n")
    out = _write_local_target("ghp_x", {"mode": "substitute", "path": str(path), "placeholder": "__PAT__"})
    assert out["written"], out
    assert path.read_text() == "token: ghp_x\nother: 1\n"


def test_substitute_errors_on_zero_or_many(tmp_path: Path) -> None:
    path = tmp_path / "c.txt"
    path.write_text("none here")
    assert not _write_local_target("v", {"mode": "substitute", "path": str(path), "placeholder": "X"})["written"]
    path.write_text("X and X")
    assert not _write_local_target("v", {"mode": "substitute", "path": str(path), "placeholder": "X"})["written"]


def test_apply_target_writes_strips_secret(tmp_path: Path) -> None:
    secret_path = tmp_path / "tok"
    note_path = tmp_path / "note"
    spec = {
        "kind": "form",
        "fields": [
            {"kind": "secret", "name": "pat", "target": {"mode": "create", "path": str(secret_path)}},
            {"kind": "text", "name": "label", "target": {"mode": "create", "path": str(note_path)}},
            {"kind": "text", "name": "plain"},
        ],
    }
    data = {
        "cancelled": False,
        "result": {"action": None, "values": {"pat": "ghp_secret", "label": "hello", "plain": "kept"}},
    }
    _apply_target_writes(spec, data)
    values = data["result"]["values"]
    # Secret: value gone, only the write outcome remains; file has the secret.
    assert "ghp_secret" not in str(values["pat"])
    assert values["pat"]["written"] is True
    assert secret_path.read_text() == "ghp_secret"
    # Non-secret target: value retained alongside the outcome; file written.
    assert values["label"]["value"] == "hello"
    assert values["label"]["written"] is True
    assert note_path.read_text() == "hello"
    # Untargeted field untouched.
    assert values["plain"] == "kept"


def test_apply_target_writes_noop_on_cancel(tmp_path: Path) -> None:
    secret_path = tmp_path / "tok"
    spec = {"kind": "form", "fields": [{"kind": "secret", "name": "pat",
            "target": {"mode": "create", "path": str(secret_path)}}]}
    data = {"cancelled": True, "result": {}}
    _apply_target_writes(spec, data)
    assert not secret_path.exists(), "no write on cancel"
