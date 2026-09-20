"""A typo in an env var must not be an MCP server that will not start (#203).

Four `float(os.environ.get(...))` calls plus `logging.basicConfig(level=…)`
run at *import* time, before FastMCP registers a single tool. So
`AIUI_TIMEOUT_S=120s`, `AIUI_COLDSTART_WAIT_S=30s` or `AIUI_LOG_LEVEL=trace`
— all natural guesses — used to raise an unhandled `ValueError`, and the host
reported only "the aiui MCP server failed to start" with the traceback buried
in a log the user may not know how to open. These are precisely the knobs
someone reaches for while debugging a flaky tunnel.

The contract: fall back to the default, and *warn* — never swallow the bad
value silently, because the user has to learn their knob was ignored.
"""
from __future__ import annotations

import logging
import os
import subprocess
import sys

import pytest

import aiui_mcp.server as server


def test_env_float_reads_a_good_value(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("AIUI_TIMEOUT_S", "45")
    assert server._env_float("AIUI_TIMEOUT_S", 120.0) == 45.0


def test_env_float_uses_the_default_when_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("AIUI_TIMEOUT_S", raising=False)
    assert server._env_float("AIUI_TIMEOUT_S", 120.0) == 120.0


def test_env_float_falls_back_on_garbage(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    """`120s` is the natural typo — a unit suffix on a knob documented in
    seconds. It must cost a warning, not the server."""
    monkeypatch.setenv("AIUI_TIMEOUT_S", "120s")
    with caplog.at_level(logging.WARNING, logger="aiui"):
        assert server._env_float("AIUI_TIMEOUT_S", 120.0) == 120.0
    assert "AIUI_TIMEOUT_S" in caplog.text
    assert "120s" in caplog.text


def test_env_log_level_accepts_a_known_level(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("AIUI_LOG_LEVEL", "debug")
    assert server._env_log_level() == "DEBUG"


def test_env_log_level_falls_back_and_warns(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    monkeypatch.setenv("AIUI_LOG_LEVEL", "trace")
    with caplog.at_level(logging.WARNING, logger="aiui"):
        assert server._env_log_level() == "INFO"
    assert "AIUI_LOG_LEVEL" in caplog.text
    assert "trace" in caplog.text


def _import_with(**env: str) -> subprocess.CompletedProcess[str]:
    """Import the module in a fresh interpreter under the given environment —
    the only way to exercise parsing that happens at import time."""
    return subprocess.run(
        [sys.executable, "-c", "import aiui_mcp.server"],
        env={**os.environ, **env},
        capture_output=True,
        text=True,
    )


def test_bad_log_level_falls_back_to_info() -> None:
    """`logging.basicConfig(level="TRACE")` raises `Unknown level` before the
    logger the warning would go to even exists. Importing must survive it."""
    proc = _import_with(AIUI_LOG_LEVEL="trace")
    assert proc.returncode == 0, proc.stderr
    assert "ValueError" not in proc.stderr
    assert "AIUI_LOG_LEVEL" in proc.stderr, "the ignored knob must be named"


@pytest.mark.parametrize(
    ("name", "value"),
    [
        ("AIUI_TIMEOUT_S", "120s"),
        ("AIUI_COLDSTART_WAIT_S", "30s"),
        ("AIUI_HEALTH_TIMEOUT_S", ""),
        ("AIUI_UPLOAD_TIMEOUT_S", "fifteen minutes"),
    ],
)
def test_bad_float_knob_never_breaks_import(name: str, value: str) -> None:
    proc = _import_with(**{name: value})
    assert proc.returncode == 0, proc.stderr
    assert "ValueError" not in proc.stderr
    assert name in proc.stderr, "the ignored knob must be named"
