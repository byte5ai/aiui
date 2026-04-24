# aiui-mcp

MCP server for [**aiui**](https://github.com/byte5ai/aiui) — lets Claude
Code sessions render native macOS dialogs on the user's Mac. Works for
local and remote Claude Code setups.

## Install

You normally don't need to touch this package directly. Install
`aiui.app` on the Mac once; it registers aiui as a global MCP server in
Claude Code's user config (`~/.claude.json`) and pulls this package via
`uvx aiui-mcp` automatically. From then on, every Claude Code session
can call `aiui.*` tools without per-project setup.

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
