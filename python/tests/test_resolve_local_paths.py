"""Bridge-side local-path resolver tests.

Mirror of the Rust resolver tests in
`companion/src-tauri/src/imageresolve.rs`. The two implementations
have to behave the same — Mac-local sessions go through the Rust
bridge, SSH-tunneled remotes through this Python one. Drift between
them produces silent "works in one setup, broken in the other"
bugs.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from aiui_mcp.server import (
    _looks_like_local_path,
    _read_path_as_data_url,
    _resolve_local_paths,
)


def test_looks_like_local_path_classifies_correctly() -> None:
    assert _looks_like_local_path("/Users/me/foo.png")
    assert _looks_like_local_path("~/Pictures/foo.png")
    assert not _looks_like_local_path("data:image/png;base64,AAAA")
    assert not _looks_like_local_path("https://a.test/x.png")
    assert not _looks_like_local_path("http://a.test/x.png")
    assert not _looks_like_local_path("./relative.png")
    assert not _looks_like_local_path("relative.png")
    assert not _looks_like_local_path("")


def test_read_path_as_data_url_uses_extension_mime(tmp_path: Path) -> None:
    f = tmp_path / "tiny.png"
    f.write_bytes(b"\x89PNG\r\n\x1a\nfake bytes")
    url = _read_path_as_data_url(str(f))
    assert url.startswith("data:image/png;base64,")


def test_read_path_as_data_url_handles_svg(tmp_path: Path) -> None:
    # SVG mime is canonicalized to image/svg+xml regardless of platform
    # mimetypes quirks.
    f = tmp_path / "icon.svg"
    f.write_bytes(b"<svg/>")
    url = _read_path_as_data_url(str(f))
    assert url.startswith("data:image/svg+xml;base64,")


def test_read_path_as_data_url_rejects_oversize(tmp_path: Path) -> None:
    from aiui_mcp.server import _MAX_IMAGE_BYTES

    f = tmp_path / "big.png"
    f.write_bytes(b"\x00" * (_MAX_IMAGE_BYTES + 1))
    with pytest.raises(ValueError, match="too large"):
        _read_path_as_data_url(str(f))


def test_read_path_as_data_url_rejects_missing(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="not a file"):
        _read_path_as_data_url(str(tmp_path / "does-not-exist.png"))


def test_resolve_local_paths_inlines_real_file_and_skips_others(tmp_path: Path) -> None:
    f = tmp_path / "icon.png"
    f.write_bytes(b"\x89PNG\r\n\x1a\nfake bytes")
    path_str = str(f)

    spec = {
        "kind": "form",
        "fields": [
            {"kind": "image", "src": path_str},
            {"kind": "image", "src": "https://leave.me/alone.png"},
            {"kind": "image", "src": "data:image/png;base64,UNCHANGED"},
            {
                "kind": "list",
                "items": [
                    {"label": "L", "value": "l", "thumbnail": path_str},
                ],
            },
        ],
    }
    _resolve_local_paths(spec)

    # Local path was rewritten in both places.
    assert spec["fields"][0]["src"].startswith("data:image/png;base64,")
    assert spec["fields"][3]["items"][0]["thumbnail"].startswith(
        "data:image/png;base64,"
    )
    # HTTPS URL is left alone — that's the server-side resolver's job.
    assert spec["fields"][1]["src"] == "https://leave.me/alone.png"
    # Pre-existing data: URL is untouched.
    assert spec["fields"][2]["src"] == "data:image/png;base64,UNCHANGED"


def test_resolve_local_paths_fails_soft_on_missing_file() -> None:
    original = "/this/path/should/not/exist/aiui-test-missing.png"
    spec = {"src": original}
    _resolve_local_paths(spec)  # should not raise
    assert spec["src"] == original


def test_resolve_local_paths_ignores_non_src_keys() -> None:
    spec = {
        "title": "/Users/looks/like/a/path/but/not/an/src.png",
        "label": "/this/is/just/text",
    }
    _resolve_local_paths(spec)
    # Neither key is `src` or `thumbnail`, so no rewrite happens — even
    # though the values would qualify if they were under the right key.
    assert spec["title"].endswith("/src.png")
    assert spec["label"] == "/this/is/just/text"
