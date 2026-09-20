# aiui-mcp

MCP server for [**aiui**](https://github.com/byte5ai/aiui) — lets Claude
Code sessions render native desktop dialogs on the user's own machine
(macOS, and Windows since aiui 0.10.1). Works for local and remote
Claude Desktop App setups.

## Install

You don't normally need to touch this package directly.

**On your own machine**, install [aiui](https://github.com/byte5ai/aiui/releases/latest)
— since v0.3.0 the MCP server is bundled as native Rust code inside the
app. `~/.claude.json` points directly at the app binary. No `uv`, no
`uvx`, no Python on the onboarding path.

**On a remote SSH host** (no aiui.app there), this package is the right
tool. aiui registers it automatically when you add the remote in
settings — `{command: "uvx", args: ["aiui-mcp"]}`. All dialogs tunnel
back through aiui on your own machine.

See the main repo for the full install flow and companion download:
<https://github.com/byte5ai/aiui>

## Tools

- `aiui.confirm` — hard yes/no with optional destructive styling
- `aiui.ask` — single- or multi-choice with per-option descriptions
- `aiui.form` — composite window with typed fields and action buttons
- `aiui.aiui_health` — reachability check

## Environment variables

None of these need setting — the defaults are what aiui registers for you.
A bad value is **warned about and ignored**, never fatal: a typo in one of
these must not turn into "the aiui MCP server failed to start".

| Variable | Default | Meaning |
| --- | --- | --- |
| `AIUI_ENDPOINT` | `http://127.0.0.1:7777` | Companion base URL — the local end of the SSH reverse-tunnel. |
| `AIUI_TOKEN_PATH` | `~/.config/aiui/token` (`%APPDATA%\aiui\token` on Windows) | Pairing token file, written by the companion and copied to each registered remote. |
| `AIUI_TIMEOUT_S` | `120` | Seconds for a held request (`/notify`, `/update`). |
| `AIUI_HEALTH_TIMEOUT_S` | `3` | Seconds for `aiui_health` and `/version` — deliberately short; it is the diagnostic. |
| `AIUI_COLDSTART_WAIT_S` | `30` | Seconds to poll `/ping` before the first call, so a companion that is still starting gets time to serve. |
| `AIUI_UPLOAD_TIMEOUT_S` | `900` | Seconds for the held `POST /upload` — generous, because the user browses a file picker inside it. |
| `AIUI_LOG_LEVEL` | `INFO` | Any standard `logging` level name. |
| `AIUI_LIVE` | unset | Test-only: set to `1` to run the integration tests against a real companion. |

## Prompts

- `/aiui:teach` — briefs the agent on aiui (full widget catalog, design rules, anti-patterns)

## License

MIT — see [LICENSE](https://github.com/byte5ai/aiui/blob/main/LICENSE).
