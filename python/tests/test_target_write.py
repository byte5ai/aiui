"""Bridge-side file-write tests (#135).

Mirror of the Rust `filewrite` tests. The bridge runs ON the agent's host, so
`target` writes are local file operations here too; secret values are written
and stripped before the result reaches the agent.
"""

from __future__ import annotations

import os
from pathlib import Path

from aiui_mcp import server
from aiui_mcp.server import (
    _action_commits_targets,
    _annotate_target_paths,
    _collect_secret_fields,
    _apply_target_writes,
    _collect_target_fields,
    _target_path_error,
    _write_local_target,
)


def test_collect_target_fields_flat_and_tabs() -> None:
    spec = {
        "kind": "form",
        "fields": [
            {"kind": "secret", "name": "a", "target": {"mode": "create", "path": "/x"}},
            {"kind": "text", "name": "b"},
        ],
        "tabs": [
            {
                "label": "T",
                "fields": [
                    {"kind": "text", "name": "c", "target": {"mode": "create", "path": "/y"}}
                ],
            }
        ],
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
    out = _write_local_target(
        "ghp_x", {"mode": "substitute", "path": str(path), "placeholder": "__PAT__"}
    )
    assert out["written"], out
    assert path.read_text() == "token: ghp_x\nother: 1\n"


def test_substitute_errors_on_zero_or_many(tmp_path: Path) -> None:
    path = tmp_path / "c.txt"
    path.write_text("none here")
    assert not _write_local_target(
        "v", {"mode": "substitute", "path": str(path), "placeholder": "X"}
    )["written"]
    path.write_text("X and X")
    assert not _write_local_target(
        "v", {"mode": "substitute", "path": str(path), "placeholder": "X"}
    )["written"]


def test_apply_target_writes_strips_secret(tmp_path: Path) -> None:
    secret_path = tmp_path / "tok"
    note_path = tmp_path / "note"
    spec = {
        "kind": "form",
        "fields": [
            {
                "kind": "secret",
                "name": "pat",
                "target": {"mode": "create", "path": str(secret_path)},
            },
            {"kind": "text", "name": "label", "target": {"mode": "create", "path": str(note_path)}},
            {"kind": "text", "name": "plain"},
        ],
    }
    data = {
        "cancelled": False,
        "result": {
            "action": None,
            "values": {"pat": "ghp_secret", "label": "hello", "plain": "kept"},
        },
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
    spec = {
        "kind": "form",
        "fields": [
            {
                "kind": "secret",
                "name": "pat",
                "target": {"mode": "create", "path": str(secret_path)},
            }
        ],
    }
    data = {"cancelled": True, "result": {}}
    _apply_target_writes(spec, data)
    assert not secret_path.exists(), "no write on cancel"


# --- issue #177: only an affirmative action commits, and never an empty value


def _spec_with_documented_actions(secret_path: Path) -> dict:
    """The Cancel / Save-draft / Create triple from docs/skill.md — the
    documented pattern that made #177 reachable by copy-paste."""
    return {
        "kind": "form",
        "fields": [
            {
                "kind": "secret",
                "name": "pat",
                "target": {
                    "mode": "create",
                    "path": str(secret_path),
                    "perm": "0600",
                    "overwrite": True,
                },
            }
        ],
        "actions": [
            {"label": "Cancel", "value": "cancel", "skip_validation": True},
            {"label": "Save draft", "value": "draft", "skip_validation": True},
            {"label": "Create", "value": "commit"},
        ],
    }


def test_write_local_target_refuses_empty_value(tmp_path: Path) -> None:
    path = tmp_path / "token"
    path.write_text("ghp_existing")
    target = {"mode": "create", "path": str(path), "perm": "0600", "overwrite": True}
    out = _write_local_target("", target)
    assert not out["written"], out
    assert "empty" in (out.get("error") or "")
    assert path.read_text() == "ghp_existing", "file untouched"


def test_substitute_refuses_empty_value(tmp_path: Path) -> None:
    path = tmp_path / "config.yaml"
    path.write_text("token: __AIUI_SECRET_PAT__\n")
    target = {"mode": "substitute", "path": str(path), "placeholder": "__AIUI_SECRET_PAT__"}
    out = _write_local_target("", target)
    assert not out["written"], out
    assert "__AIUI_SECRET_PAT__" in path.read_text(), "sentinel survives, retry possible"


def test_action_commits_targets_rules() -> None:
    spec = _spec_with_documented_actions(Path("/tmp/x"))
    assert _action_commits_targets(spec, None) is True  # built-in submit
    assert _action_commits_targets(spec, "commit") is True  # plain named action
    assert _action_commits_targets(spec, "cancel") is False  # skip_validation
    assert _action_commits_targets(spec, "draft") is False  # skip_validation
    assert _action_commits_targets(spec, "nope") is False  # unknown → fail closed
    assert _action_commits_targets({"kind": "form"}, "x") is False  # no action list
    override = {
        "actions": [
            {"label": "Force", "value": "f", "skip_validation": True, "writes_targets": True}
        ]
    }
    assert _action_commits_targets(override, "f") is True


def test_apply_target_writes_skips_skip_validation_action(tmp_path: Path) -> None:
    secret_path = tmp_path / "tok"
    secret_path.write_text("ghp_existing")
    spec = _spec_with_documented_actions(secret_path)
    data = {"cancelled": False, "result": {"action": "cancel", "values": {"pat": "ghp_real"}}}
    _apply_target_writes(spec, data)
    assert secret_path.read_text() == "ghp_existing", "Cancel must not write"
    values = data["result"]["values"]
    assert values["pat"]["written"] is False
    assert "does not commit" in values["pat"]["error"]
    assert "ghp_real" not in str(values["pat"]), "secret still stripped"


def test_apply_target_writes_commits_on_primary_action(tmp_path: Path) -> None:
    secret_path = tmp_path / "tok"
    spec = _spec_with_documented_actions(secret_path)
    data = {"cancelled": False, "result": {"action": "commit", "values": {"pat": "ghp_real"}}}
    _apply_target_writes(spec, data)
    assert secret_path.read_text() == "ghp_real", "happy path must not regress"
    values = data["result"]["values"]
    assert values["pat"]["written"] is True
    assert "ghp_real" not in str(values["pat"])


def test_apply_target_writes_missing_value_writes_nothing(tmp_path: Path) -> None:
    secret_path = tmp_path / "tok"
    spec = _spec_with_documented_actions(secret_path)
    data = {"cancelled": False, "result": {"action": "commit", "values": {}}}
    _apply_target_writes(spec, data)
    assert not secret_path.exists(), "absent field must not be laundered into an empty write"
    assert data["result"]["values"]["pat"]["written"] is False
    assert "no value submitted" in data["result"]["values"]["pat"]["error"]


def test_apply_target_writes_blank_value_leaves_file_intact(tmp_path: Path) -> None:
    """The #177 headline: user leaves an optional secret blank, presses the
    primary button — the existing credential file must survive."""
    secret_path = tmp_path / "tok"
    secret_path.write_text("ghp_existing")
    spec = _spec_with_documented_actions(secret_path)
    data = {"cancelled": False, "result": {"action": "commit", "values": {"pat": ""}}}
    _apply_target_writes(spec, data)
    assert secret_path.read_text() == "ghp_existing"
    assert data["result"]["values"]["pat"]["written"] is False


# --- issue #186: a `secret` is write-only by KIND, with or without a target


def test_collect_secret_fields_finds_them_with_and_without_target() -> None:
    spec = {
        "kind": "form",
        "fields": [
            {"kind": "secret", "name": "bare"},
            {"kind": "secret", "name": "targeted", "target": {"mode": "create", "path": "/x"}},
            {"kind": "password", "name": "pw"},
            {"kind": "text", "name": "t"},
        ],
        "tabs": [{"label": "T", "fields": [{"kind": "secret", "name": "in_tab"}]}],
    }
    assert sorted(_collect_secret_fields(spec)) == ["bare", "in_tab", "targeted"]


def test_target_less_secret_is_never_returned_to_the_agent(tmp_path: Path) -> None:
    """The #186 headline. An older companion does not reject this shape, so a
    newer bridge in front of one must not hand the plaintext back."""
    spec = {
        "kind": "form",
        "fields": [
            {"kind": "secret", "name": "pat", "label": "GitHub PAT"},
            {"kind": "text", "name": "note"},
        ],
    }
    data = {
        "cancelled": False,
        "result": {"action": None, "values": {"pat": "ghp_live_secret", "note": "kept"}},
    }
    _apply_target_writes(spec, data)
    values = data["result"]["values"]
    assert "ghp_live_secret" not in str(values), "the plaintext must not survive"
    assert values["pat"]["written"] is False
    assert "no target" in values["pat"]["error"]
    assert values["note"] == "kept", "non-secret fields are untouched"


def test_target_less_secret_inside_a_tab_is_stripped(tmp_path: Path) -> None:
    spec = {
        "kind": "form",
        "tabs": [{"label": "Creds", "fields": [{"kind": "secret", "name": "pat"}]}],
    }
    data = {"cancelled": False, "result": {"action": None, "values": {"pat": "ghp_x"}}}
    _apply_target_writes(spec, data)
    assert "ghp_x" not in str(data["result"]["values"])


def test_targeted_secret_still_reports_its_write(tmp_path: Path) -> None:
    """The happy path must not regress: a real write still wins over the
    fallback marker."""
    secret_path = tmp_path / "tok"
    spec = {
        "kind": "form",
        "fields": [
            {
                "kind": "secret",
                "name": "pat",
                "target": {"mode": "create", "path": str(secret_path)},
            }
        ],
    }
    data = {"cancelled": False, "result": {"action": None, "values": {"pat": "ghp_real"}}}
    _apply_target_writes(spec, data)
    assert secret_path.read_text() == "ghp_real"
    assert data["result"]["values"]["pat"]["written"] is True
    assert "no target" not in str(data["result"]["values"]["pat"])


def test_password_field_is_still_returned(tmp_path: Path) -> None:
    """`password` is the documented alternative for "I want the value back" —
    it must keep working, or agents have nowhere to go."""
    spec = {"kind": "form", "fields": [{"kind": "password", "name": "pw"}]}
    data = {"cancelled": False, "result": {"action": None, "values": {"pw": "hunter2"}}}
    _apply_target_writes(spec, data)
    assert data["result"]["values"]["pw"] == "hunter2"


def test_cancelled_dialog_with_a_bare_secret_is_still_a_noop(tmp_path: Path) -> None:
    spec = {"kind": "form", "fields": [{"kind": "secret", "name": "pat"}]}
    data = {"cancelled": True, "result": {}}
    _apply_target_writes(spec, data)
    assert data == {"cancelled": True, "result": {}}, "cancel carries no values"


def test_absent_secret_value_is_not_invented(tmp_path: Path) -> None:
    """A secret the user never filled in has no key in `values`; the strip
    must not create one."""
    spec = {"kind": "form", "fields": [{"kind": "secret", "name": "pat"}]}
    data = {"cancelled": False, "result": {"action": None, "values": {}}}
    _apply_target_writes(spec, data)
    assert "pat" not in data["result"]["values"]


# --- issue #199: the two bridges must agree, and nothing may escape as an
# exception after the user has already typed (and lost) a credential


def test_substitute_preserves_existing_mode(tmp_path: Path) -> None:
    """The headline: a 0644 compose file came back 0600 and the container
    reading it as another uid stopped starting — reported as `written: true`,
    with nothing in the outcome naming the change."""
    path = tmp_path / "docker-compose.yml"
    path.write_text("token: __PAT__\n")
    os.chmod(path, 0o644)
    out = _write_local_target(
        "ghp_x", {"mode": "substitute", "path": str(path), "placeholder": "__PAT__"}
    )
    assert out["written"], out
    assert (path.stat().st_mode & 0o777) == 0o644, "substitute keeps the file's own mode"
    assert out["mode"] == "0644", "and the outcome says which mode was applied"

    # An explicit `perm` still wins.
    path.write_text("token: __PAT__\n")
    out2 = _write_local_target(
        "ghp_y", {"mode": "substitute", "path": str(path), "placeholder": "__PAT__", "perm": "0640"}
    )
    assert out2["written"], out2
    assert (path.stat().st_mode & 0o777) == 0o640
    assert out2["mode"] == "0640"


def test_create_defaults_to_0600_without_perm(tmp_path: Path) -> None:
    """Pins tight-by-default against a naive "drop the 0600" fix for the
    substitute bug — that would give a fresh credential file the umask."""
    path = tmp_path / "token"
    out = _write_local_target("ghp_secret", {"mode": "create", "path": str(path)})
    assert out["written"], out
    assert (path.stat().st_mode & 0o777) == 0o600
    assert out["mode"] == "0600"


def test_substitute_writes_through_symlink(tmp_path: Path) -> None:
    """Before: the read followed the link, `os.replace` landed ON it. The link
    became a regular file holding the secret and the real config kept its
    untouched placeholder — reported as a successful write."""
    real = tmp_path / "real.yml"
    link = tmp_path / "link.yml"
    real.write_text("token: __PAT__\n")
    link.symlink_to(real)
    out = _write_local_target(
        "ghp_x", {"mode": "substitute", "path": str(link), "placeholder": "__PAT__"}
    )
    assert out["written"], out
    assert os.path.islink(link), "the link must survive as a link"
    assert real.read_text() == "token: ghp_x\n", "the real file is the one edited"
    assert out["target"] == str(real.resolve()), "the outcome names the real destination"


def test_substitute_non_utf8_target_returns_error(tmp_path: Path) -> None:
    """A latin-1 `.ini` raised UnicodeDecodeError — a ValueError, so it slipped
    past `except OSError` and killed the tool call with a traceback."""
    path = tmp_path / "app.ini"
    original = "user=André\ntoken=__PAT__\n".encode("latin-1")
    path.write_bytes(original)
    out = _write_local_target(
        "ghp_x", {"mode": "substitute", "path": str(path), "placeholder": "__PAT__"}
    )
    assert out["written"] is False
    assert out.get("error"), "a structured error, not an exception"
    assert path.read_bytes() == original, "the file is left byte-identical"


def test_substitute_preserves_non_ascii_bytes(tmp_path: Path, monkeypatch) -> None:
    """A UTF-8 file round-trips byte-identical apart from the placeholder even
    when the host locale is not UTF-8. The old code read with the locale
    encoding and wrote back UTF-8, mangling every non-ASCII byte in a file the
    user asked to touch on exactly one line."""
    import locale

    monkeypatch.setattr(locale, "getpreferredencoding", lambda *a, **k: "latin-1")
    path = tmp_path / "conf"
    path.write_bytes("user=André\ntoken=__PAT__\n".encode())
    out = _write_local_target(
        "ghp_x", {"mode": "substitute", "path": str(path), "placeholder": "__PAT__"}
    )
    assert out["written"], out
    assert path.read_bytes() == "user=André\ntoken=ghp_x\n".encode()


def test_write_without_fchmod_does_not_crash(tmp_path: Path, monkeypatch) -> None:
    """The Windows bridge host: `os.fchmod` does not exist there, and the
    unguarded call made *every* target write die with AttributeError."""
    monkeypatch.delattr(os, "fchmod", raising=True)
    path = tmp_path / "token"
    out = _write_local_target("ghp_x", {"mode": "create", "path": str(path)})
    assert out["written"] is True, out
    assert path.read_text() == "ghp_x"
    assert "mode" not in out, "no mode bits to report off POSIX"


def test_malformed_target_returns_structured_error() -> None:
    """`"target": "~/.github_tokens/byte5ai"` (a string, not an object) used to
    raise `AttributeError: 'str' object has no attribute 'get'`."""
    out = _write_local_target("v", "not-a-dict")
    assert out == {
        "written": False,
        "target": "",
        "bytes": 0,
        "error": "target must be an object with mode/path",
    }


def test_collect_target_fields_skips_non_dict_target() -> None:
    spec = {
        "kind": "form",
        "fields": [
            {"kind": "secret", "name": "str_target", "target": "~/x"},
            {"kind": "text", "name": "ok", "target": {"mode": "create", "path": "/x"}},
        ],
    }
    assert [f["name"] for f in _collect_target_fields(spec)] == ["ok"]


def test_relative_and_tilde_user_paths_rejected(tmp_path: Path, monkeypatch) -> None:
    """The two bridges resolved these differently — the agent's cwd here, the
    GUI's cwd (typically `/`) there; `~alice/` expanded here, a literal
    directory there. Neither destination is the one the user approved."""
    monkeypatch.chdir(tmp_path)
    for bad in ("notes/key", "~alice/key", "./key"):
        out = _write_local_target("v", {"mode": "create", "path": bad})
        assert out["written"] is False, bad
        assert "absolute or ~/-rooted" in out["error"], out
    assert not (tmp_path / "notes").exists(), "nothing written anywhere"
    # The accepted shapes still work.
    assert _write_local_target("v", {"mode": "create", "path": str(tmp_path / "a")})["written"]
    assert _target_path_error("~/x") is None


def test_apply_target_writes_never_raises(tmp_path: Path, monkeypatch) -> None:
    """One bad field used to abort the loop, so the writes that had already
    landed were never reported and the call died with a traceback."""
    good = tmp_path / "good"
    spec = {
        "kind": "form",
        "fields": [
            {"kind": "text", "name": "a", "target": {"mode": "create", "path": str(good)}},
            {"kind": "text", "name": "b", "target": {"mode": "create", "path": "relative/key"}},
        ],
    }
    data = {"cancelled": False, "result": {"action": None, "values": {"a": "one", "b": "two"}}}
    _apply_target_writes(spec, data)
    values = data["result"]["values"]
    assert good.read_text() == "one", "the good field still lands"
    assert values["a"]["written"] is True
    assert values["b"]["written"] is False and values["b"]["error"]

    # …and even an unexpected exception from the writer is folded into an
    # outcome rather than propagating out of the submit path.
    def boom(*_a: object, **_k: object) -> dict:
        raise RuntimeError("disk on fire")

    monkeypatch.setattr(server, "_write_local_target", boom)
    data2 = {"cancelled": False, "result": {"action": None, "values": {"a": "one", "b": "two"}}}
    _apply_target_writes(spec, data2)
    assert data2["result"]["values"]["a"]["written"] is False
    assert "disk on fire" in data2["result"]["values"]["a"]["error"]


def test_annotate_target_paths_stamps_the_destination(tmp_path: Path) -> None:
    """The approval line showed the raw spec string. It now shows what this
    host — the one that performs the write — will actually write."""
    real = tmp_path / "real.yml"
    link = tmp_path / "link.yml"
    real.write_text("x")
    link.symlink_to(real)
    spec = {
        "kind": "form",
        "fields": [
            {"kind": "secret", "name": "pat", "target": {"mode": "create", "path": "~/x"}},
            {"kind": "text", "name": "plain"},
        ],
        "tabs": [
            {
                "label": "T",
                "fields": [
                    {
                        "kind": "text",
                        "name": "b",
                        "target": {"mode": "substitute", "path": str(link), "placeholder": "x"},
                    },
                ],
            }
        ],
    }
    _annotate_target_paths(spec)
    resolved = spec["fields"][0]["target"]["resolved_path"]
    assert not resolved.startswith("~"), resolved
    assert resolved == str(Path("~/x").expanduser())
    assert "target" not in spec["fields"][1], "untargeted field untouched"
    assert spec["tabs"][0]["fields"][0]["target"]["resolved_path"] == str(real.resolve()), (
        "a symlinked target shows the file that really changes"
    )
