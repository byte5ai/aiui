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
    _collect_local_audios,
    _collect_local_videos,
    _is_local_audio,
    _is_local_video,
    _looks_like_local_path,
    _read_path_as_data_url,
    _replace_srcs,
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


def test_resolve_local_paths_walks_confirm_image_and_ask_thumbnail(tmp_path: Path) -> None:
    """`confirm.image.src` and `ask.options[].thumbnail` are new image slots
    in 0.4.23. The resolver walks any `src`/`thumbnail` key regardless of
    tool spec — pin that down so a future refactor can't narrow it.
    """
    f = tmp_path / "tiny.png"
    f.write_bytes(b"\x89PNG\r\n\x1a\nfake bytes")
    path_str = str(f)

    confirm_spec = {
        "kind": "confirm",
        "title": "OK?",
        "image": {"src": path_str},
    }
    _resolve_local_paths(confirm_spec)
    assert confirm_spec["image"]["src"].startswith("data:image/png;base64,")

    ask_spec = {
        "kind": "ask",
        "question": "Which?",
        "options": [
            {"label": "A", "thumbnail": path_str},
            {"label": "B", "thumbnail": "https://leave.me/b.png"},
            {"label": "C"},
        ],
    }
    _resolve_local_paths(ask_spec)
    assert ask_spec["options"][0]["thumbnail"].startswith("data:image/png;base64,")
    assert ask_spec["options"][1]["thumbnail"] == "https://leave.me/b.png"
    assert "thumbnail" not in ask_spec["options"][2]


def test_resolve_local_paths_walks_gallery_items(tmp_path: Path) -> None:
    """Gallery `items[].src` must resolve the same way — local image and
    video paths inline as data:, remote/data URLs pass through.
    """
    img = tmp_path / "shot.png"
    img.write_bytes(b"\x89PNG\r\n\x1a\nfake bytes")
    vid = tmp_path / "clip.mp4"
    vid.write_bytes(b"\x00\x00\x00\x18ftypmp42fake")

    gallery_spec = {
        "kind": "gallery",
        "items": [
            {"value": "a", "src": str(img)},
            {"value": "b", "src": str(vid)},
            {"value": "c", "src": "https://leave.me/c.png"},
            {"value": "d", "src": "data:image/png;base64,UNCHANGED"},
        ],
    }
    _resolve_local_paths(gallery_spec)
    assert gallery_spec["items"][0]["src"].startswith("data:image/png;base64,")
    assert gallery_spec["items"][1]["src"].startswith("data:video/mp4;base64,")
    assert gallery_spec["items"][2]["src"] == "https://leave.me/c.png"
    assert gallery_spec["items"][3]["src"] == "data:image/png;base64,UNCHANGED"


def test_resolve_local_paths_walks_compare_variants(tmp_path: Path) -> None:
    """Compare `variants[].src` must resolve the same generic way as every
    other `src`/`thumbnail` slot — local image path inlines as data:,
    remote/data URLs pass through untouched.
    """
    img = tmp_path / "draft-a.png"
    img.write_bytes(b"\x89PNG\r\n\x1a\nfake bytes")

    compare_spec = {
        "kind": "compare",
        "variants": [
            {"value": "a", "src": str(img)},
            {"value": "b", "src": "https://leave.me/b.png"},
            {"value": "c", "content": "Just markdown text, no src."},
        ],
    }
    _resolve_local_paths(compare_spec)
    assert compare_spec["variants"][0]["src"].startswith("data:image/png;base64,")
    assert compare_spec["variants"][1]["src"] == "https://leave.me/b.png"
    assert "src" not in compare_spec["variants"][2]


def test_is_local_video_classifies_correctly() -> None:
    assert _is_local_video("/Users/me/clip.mp4")
    assert _is_local_video("~/Movies/take.MOV")
    assert _is_local_video("/tmp/a.webm")
    assert _is_local_video("/tmp/a.m4v")
    assert not _is_local_video("https://x.test/clip.mp4")
    assert not _is_local_video("data:video/mp4;base64,AAAA")
    assert not _is_local_video("/Users/me/photo.png")
    assert not _is_local_video("relative/clip.mp4")


def test_collect_and_replace_local_videos_mirrors_rust() -> None:
    spec = {
        "kind": "gallery",
        "items": [
            {"value": "a", "src": "/Users/me/one.mp4"},
            {"value": "b", "src": "https://x.test/two.mp4"},
            {"value": "c", "src": "/Users/me/pic.png"},
            {"value": "d", "thumbnail": "/Users/me/one.mp4"},
        ],
    }
    found: list[str] = []
    _collect_local_videos(spec, found)
    # De-duplicated: the same path in two slots appears once.
    assert found == ["/Users/me/one.mp4"]

    mapping = {"/Users/me/one.mp4": "http://127.0.0.1:7777/media/blob/x.mp4"}
    _replace_srcs(spec, mapping)
    assert spec["items"][0]["src"] == "http://127.0.0.1:7777/media/blob/x.mp4"
    assert spec["items"][3]["thumbnail"] == "http://127.0.0.1:7777/media/blob/x.mp4"
    # Untouched: https video and the image.
    assert spec["items"][1]["src"] == "https://x.test/two.mp4"
    assert spec["items"][2]["src"] == "/Users/me/pic.png"


def test_read_path_as_data_url_uses_audio_mime_overrides(tmp_path: Path) -> None:
    for ext, mime in (
        ("mp3", "audio/mpeg"),
        ("m4a", "audio/mp4"),
        ("wav", "audio/wav"),
        ("aac", "audio/aac"),
        ("ogg", "audio/ogg"),
        ("flac", "audio/flac"),
    ):
        f = tmp_path / f"sample.{ext}"
        f.write_bytes(b"fake audio bytes")
        url = _read_path_as_data_url(str(f))
        assert url.startswith(f"data:{mime};base64,"), f"{ext}: {url}"


def test_is_local_audio_classifies_correctly() -> None:
    assert _is_local_audio("/Users/me/sample.mp3")
    assert _is_local_audio("~/Music/voice.M4A")
    assert _is_local_audio("/tmp/a.wav")
    assert _is_local_audio("/tmp/a.aac")
    assert _is_local_audio("/tmp/a.ogg")
    assert _is_local_audio("/tmp/a.flac")
    assert not _is_local_audio("https://x.test/clip.mp3")
    assert not _is_local_audio("data:audio/mpeg;base64,AAAA")
    assert not _is_local_audio("/Users/me/photo.png")
    assert not _is_local_audio("/Users/me/clip.mp4")  # video, not audio
    assert not _is_local_audio("relative/clip.mp3")


def test_collect_and_replace_local_audio_mirrors_rust() -> None:
    spec = {
        "kind": "form",
        "fields": [
            {"kind": "audio", "src": "/Users/me/sample.mp3"},
            {"kind": "audio", "src": "https://x.test/two.mp3"},
            {"kind": "image", "src": "/Users/me/pic.png"},
            {
                "kind": "list",
                "items": [{"label": "L", "value": "l", "thumbnail": "/Users/me/sample.mp3"}],
            },
        ],
    }
    found: list[str] = []
    _collect_local_audios(spec, found)
    # De-duplicated: the same path in two slots appears once.
    assert found == ["/Users/me/sample.mp3"]

    mapping = {"/Users/me/sample.mp3": "http://127.0.0.1:7777/media/blob/x.mp3"}
    _replace_srcs(spec, mapping)
    assert spec["fields"][0]["src"] == "http://127.0.0.1:7777/media/blob/x.mp3"
    assert (
        spec["fields"][3]["items"][0]["thumbnail"]
        == "http://127.0.0.1:7777/media/blob/x.mp3"
    )
    # Untouched: https audio and the image.
    assert spec["fields"][1]["src"] == "https://x.test/two.mp3"
    assert spec["fields"][2]["src"] == "/Users/me/pic.png"
