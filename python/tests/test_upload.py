"""Bridge-side upload tests (#146).

Cover the pure helpers behind the `upload` tool — filename sanitisation,
target-dir expansion, and the no-clobber atomic write — without needing a
running companion. Mirrors the Rust bridge's `do_upload` helpers.
"""

from __future__ import annotations

import asyncio
import errno
import os
import urllib.parse
from pathlib import Path
from typing import Any

import httpx
import pytest

import aiui_mcp.server as server
from aiui_mcp.server import (
    _upload_expand_dir,
    _upload_safe_base_name,
    _upload_write,
    upload,
)


def test_safe_base_name_strips_directories() -> None:
    assert _upload_safe_base_name("report.pdf") == "report.pdf"
    assert _upload_safe_base_name("/Users/me/Downloads/report.pdf") == "report.pdf"
    assert _upload_safe_base_name("../../etc/passwd") == "passwd"
    assert _upload_safe_base_name("  spaced.txt  ") == "spaced.txt"


def test_safe_base_name_rejects_empty_and_dots() -> None:
    assert _upload_safe_base_name("") is None
    assert _upload_safe_base_name(".") is None
    assert _upload_safe_base_name("..") is None
    assert _upload_safe_base_name("/") is None


def test_filename_header_roundtrip() -> None:
    # The companion percent-encodes UTF-8 filenames into an ASCII header;
    # the bridge decodes with unquote_to_bytes → utf-8. Prove a name with an
    # umlaut and a space survives.
    original = "Prüfung final.md"
    encoded = urllib.parse.quote(original, safe="")
    assert encoded.isascii()
    decoded = urllib.parse.unquote_to_bytes(encoded).decode("utf-8", "replace")
    assert _upload_safe_base_name(decoded) == original


@pytest.mark.skipif(
    os.name == "nt",
    reason="POSIX absolute-path fixture (`/tmp/x`) is not absolute on Windows",
)
def test_expand_dir_absolute_and_tilde() -> None:
    assert _upload_expand_dir("/tmp/x") == Path("/tmp/x")
    assert _upload_expand_dir("~/Downloads") == Path.home() / "Downloads"
    assert _upload_expand_dir("~") == Path.home()
    # Relative paths are rejected — no stable cwd contract.
    assert _upload_expand_dir("relative/dir") is None
    assert _upload_expand_dir("./here") is None
    # #194: `~user` is not a form this bridge expands — and it must not reach
    # `expanduser()`, which raises RuntimeError for an unknown user and so
    # escaped the tool's `{status: "error"}` contract entirely. Byte-for-byte
    # the Rust `expand_dir` rule: only `~` and `~/…`.
    assert _upload_expand_dir("~nosuchuser12345/x") is None
    assert _upload_expand_dir("~root") is None


def test_write_falls_back_without_hardlink_support(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # #194: exFAT/FAT32/SMB destinations have no hard links, so `os.link`
    # fails with something that is not FileExistsError — after the bytes have
    # already crossed the tunnel. The file must still land, exactly once.
    def no_link(src: Any, dst: Any) -> None:
        raise OSError(errno.EPERM, "operation not permitted")

    monkeypatch.setattr(os, "link", no_link)

    out = _upload_write(tmp_path, "stick.bin", b"payload")
    assert out["status"] == "ok"
    assert out["bytes"] == 7
    assert (tmp_path / "stick.bin").read_bytes() == b"payload"
    names = sorted(p.name for p in tmp_path.iterdir())
    assert names == ["stick.bin"], f"written once, no temp left: {names}"

    # No-clobber survives the fallback: O_EXCL refuses the same way the link
    # would have.
    clash = _upload_write(tmp_path, "stick.bin", b"other")
    assert clash["status"] == "error"
    assert "already exists" in clash["error"]
    assert (tmp_path / "stick.bin").read_bytes() == b"payload"


def test_upload_returns_error_dict_when_cwd_unavailable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # #194: with no `target_dir`, `upload` defaults to the process cwd — and
    # `Path.cwd()` raises when that directory has been removed. Every failure
    # of this tool is documented to come back as `{status: "error", error}`,
    # never as an exception.
    def no_cwd() -> Path:
        raise FileNotFoundError(errno.ENOENT, "No such file or directory")

    monkeypatch.setattr(server.Path, "cwd", staticmethod(no_cwd))

    out = asyncio.run(upload())
    assert out["status"] == "error"
    assert "cwd unavailable" in out["error"]


def test_write_creates_file(tmp_path: Path) -> None:
    out = _upload_write(tmp_path, "hello.txt", b"hi there")
    assert out["status"] == "ok"
    assert out["filename"] == "hello.txt"
    assert out["bytes"] == 8
    dest = tmp_path / "hello.txt"
    assert dest.read_bytes() == b"hi there"
    assert out["path"] == str(dest)


def test_write_refuses_to_clobber(tmp_path: Path) -> None:
    dest = tmp_path / "existing.txt"
    dest.write_text("original")
    out = _upload_write(tmp_path, "existing.txt", b"new content")
    assert out["status"] == "error"
    assert "already exists" in out["error"]
    # The existing file is untouched.
    assert dest.read_text() == "original"


def test_write_leaves_no_temp_files(tmp_path: Path) -> None:
    _upload_write(tmp_path, "a.bin", b"\x00\x01\x02")
    names = sorted(p.name for p in tmp_path.iterdir())
    assert names == ["a.bin"], f"stray temp files: {names}"


@pytest.mark.skipif(os.name == "nt", reason="POSIX mode bits")
@pytest.mark.parametrize("hard_links", [True, False], ids=["link", "no-link-fallback"])
def test_uploaded_file_is_owner_only_on_every_write_path(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, hard_links: bool
) -> None:
    """C-09: an upload may be a credential. The common path (mkstemp + link)
    landed it 0600, but the exFAT/SMB fallback `os.open`ed it with no mode —
    `0o777 & ~umask`, i.e. 0755 on a typical host: world-readable and
    executable. The umask is pinned to 022 so a strict runner umask cannot
    make the old code pass by accident."""
    if not hard_links:

        def no_link(src: Any, dst: Any) -> None:
            raise OSError(errno.EPERM, "operation not permitted")

        monkeypatch.setattr(os, "link", no_link)
    old_umask = os.umask(0o022)
    try:
        out = _upload_write(tmp_path, "creds.env", b"TOKEN=x")
    finally:
        os.umask(old_umask)
    assert out["status"] == "ok", out
    mode = (tmp_path / "creds.env").stat().st_mode & 0o777
    assert mode == 0o600, f"{mode:04o}"


def _upload_companion(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """A companion that answers `POST /upload` with a picked file, no network."""
    token_file = tmp_path / "token"
    token_file.write_text("de1e7e57" * 8)
    monkeypatch.setattr(server, "TOKEN_PATH", token_file)

    async def noop(*args: Any, **kwargs: Any) -> None:
        return None

    monkeypatch.setattr(server, "_wait_for_aiui", noop)
    monkeypatch.setattr(server, "_preflight", noop)

    async def fake_post(self: Any, url: str, **kwargs: Any) -> Any:
        return httpx.Response(
            200,
            content=b"payload",
            headers={"x-aiui-filename": "picked.txt"},
            request=httpx.Request("POST", url),
        )

    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)


def test_upload_write_runs_off_the_event_loop(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """E-08: writing and fsyncing up to 512 MB inside `async def upload` blocked
    the event loop — stdin (an Esc, a `ping`) went unread meanwhile."""
    _upload_companion(monkeypatch, tmp_path)
    dest = tmp_path / "in"
    dest.mkdir()
    on_loop: list[bool] = []
    real_write = server._upload_write

    def recording_write(*args: Any) -> dict[str, Any]:
        try:
            asyncio.get_running_loop()
            on_loop.append(True)
        except RuntimeError:
            on_loop.append(False)
        return real_write(*args)

    monkeypatch.setattr(server, "_upload_write", recording_write)
    out = asyncio.run(upload(target_dir=str(dest)))
    assert out["status"] == "ok", out
    assert (dest / "picked.txt").read_bytes() == b"payload"
    assert on_loop == [False], "the write must run in a worker thread"


def test_upload_reports_a_missing_token_as_a_status_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """E-09: `upload` documents `{status: "error", error}` for every failure,
    but `_preflight`/`_token` raise RuntimeError (no token, unreachable
    companion, 401) and that escaped as a tool error instead."""
    monkeypatch.setattr(server, "TOKEN_PATH", tmp_path / "no-such-token")

    async def noop(*args: Any, **kwargs: Any) -> None:
        return None

    monkeypatch.setattr(server, "_wait_for_aiui", noop)
    out = asyncio.run(upload(target_dir=str(tmp_path)))
    assert out["status"] == "error"
    assert "token not found" in out["error"]
