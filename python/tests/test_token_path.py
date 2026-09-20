"""Per-OS default location of the pairing token (#210).

`_default_token_path` is the only platform switch in the bridge, and until
the `mcp` CI job grew a Windows leg its `win32` branch was never executed
anywhere — a wrong path there is a silent auth failure (the bridge reads no
token and every tool call comes back unauthorised), not a crash that would
show up in a log.

The assertions are separator-agnostic on purpose: these run on Linux, macOS
*and* Windows, where `Path` renders the very same result with backslashes.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from aiui_mcp import server


def test_default_token_path_windows_uses_appdata(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    appdata = tmp_path / "Roaming"
    monkeypatch.setattr(server.sys, "platform", "win32")
    monkeypatch.setenv("APPDATA", str(appdata))

    p = Path(server._default_token_path())

    assert p == appdata / "aiui" / "token"
    assert p.parts[-2:] == ("aiui", "token")


def test_default_token_path_windows_falls_back_without_appdata(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # %APPDATA% unset is unusual but not impossible (a service account, a
    # stripped-down shell); the fallback must still land inside the profile.
    monkeypatch.setattr(server.sys, "platform", "win32")
    monkeypatch.delenv("APPDATA", raising=False)
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))

    p = Path(server._default_token_path())

    assert p == tmp_path / "AppData" / "Roaming" / "aiui" / "token"
    assert p.parts[-4:] == ("AppData", "Roaming", "aiui", "token")


@pytest.mark.parametrize("platform", ["linux", "darwin"])
def test_default_token_path_is_xdg_style_off_windows(
    monkeypatch: pytest.MonkeyPatch, platform: str
) -> None:
    # macOS deliberately shares the Linux path so v0.4.x installs need no
    # migration — an %APPDATA%-shaped answer here would break every existing
    # pairing.
    monkeypatch.setattr(server.sys, "platform", platform)
    monkeypatch.setenv("APPDATA", r"C:\should-be-ignored")

    assert server._default_token_path() == "~/.config/aiui/token"
