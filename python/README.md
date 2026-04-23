# aiui-mcp

MCP server for [**aiui**](https://github.com/byte5ai/aiui) — lets Claude Code
sessions render native macOS dialogs on the user's Mac. Works for local and
remote Claude Code setups.

## Install

Drop this into your project's `.mcp.json`:

```json
{
  "mcpServers": {
    "aiui": {
      "command": "uvx",
      "args": ["aiui-mcp"]
    }
  }
}
```

`uvx` pulls the latest version automatically. The companion macOS app is
distributed separately from <https://github.com/byte5ai/aiui/releases>.

## Tools

- `aiui.confirm` — hard yes/no with optional destructive styling
- `aiui.ask` — single- or multi-choice with per-option descriptions
- `aiui.form` — composite window with typed fields and action buttons
- `aiui.aiui_health` — reachability check

## Prompts

- `/aiui:widgets` — full widget catalog with rules, patterns, anti-patterns

## License

MIT — see [LICENSE](https://github.com/byte5ai/aiui/blob/main/LICENSE).
