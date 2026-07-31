"""Bridge-side upload tests (#146).

Cover the pure helpers behind the `upload` tool — filename sanitisation,
target-dir expansion, and the no-clobber atomic write — without needing a
running companion. Mirrors the Rust bridge's `do_upload` helpers.
"""
from __future__ import annotations

import urllib.parse
from pathlib import Path

from aiui_mcp.server import (
    _upload_expand_dir,
    _upload_safe_base_name,
    _upload_write,
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


def test_expand_dir_absolute_and_tilde() -> None:
    assert _upload_expand_dir("/tmp/x") == Path("/tmp/x")
    assert _upload_expand_dir("~/Downloads") == Path.home() / "Downloads"
    # Relative paths are rejected — no stable cwd contract.
    assert _upload_expand_dir("relative/dir") is None
    assert _upload_expand_dir("./here") is None


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
