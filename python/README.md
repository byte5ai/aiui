# aiui-mcp

MCP server for [**aiui**](https://github.com/byte5ai/aiui) — lets Claude
Code sessions render native macOS dialogs on the user's Mac. Works for
local and remote Claude Code setups.

## Install

You don't normally need to touch this package directly.

**On your Mac**, install [`aiui.app`](https://github.com/byte5ai/aiui/releases/latest)
— since v0.3.0 the MCP server is bundled as native Rust code inside the
app. `~/.claude.json` points directly at the app binary. No `uv`, no
`uvx`, no Python on the onboarding path.

**On a remote SSH host** (no aiui.app there), this package is the right
tool. aiui registers it automatically when you add the remote in
settings — `{command: "uvx", args: ["aiui-mcp"]}`. All dialogs tunnel
back through aiui on your Mac.

See the main repo for the full install flow and companion download:
<https://github.com/byte5ai/aiui>

## Tools

- `aiui.confirm` — hard yes/no with optional destructive styling
- `aiui.ask` — single- or multi-choice with per-option descriptions
- `aiui.form` — composite window with typed fields and action buttons
- `aiui.aiui_health` — reachability check

## Prompts

- `/aiui:widgets` — full widget catalog with rules, patterns, anti-patterns

## License

MIT — see [LICENSE](https://github.com/byte5ai/aiui/blob/main/LICENSE).
