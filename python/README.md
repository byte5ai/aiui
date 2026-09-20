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

- `confirm` — hard yes/no with optional destructive styling
- `ask` — single- or multi-choice with per-option descriptions
- `form` — composite window with typed fields and action buttons
- `gallery` — image/video grid the user picks from
- `compare` — side-by-side diff of two texts or images
- `upload` — native file picker on the user's machine; the chosen file
  lands on the agent host
- `notify` — fire-and-forget OS notification, no dialog, no reply
- `aiui_health` — reachability check
- `version` — companion version, build info, binary path, updater endpoint
- `update` — silent update check + install on the user's machine

## Prompts

- `/aiui:teach` — briefs the agent on aiui (full widget catalog, design rules, anti-patterns)
- `/aiui:update` — agent calls the `update` tool and reports the outcome
- `/aiui:version` — reports the installed aiui version in one line
- `/aiui:health` — one-line health check of the companion
- `/aiui:test-dialog` — pops a demo dialog to verify the wiring end to end
- `/aiui:remotes` — lists the registered aiui remotes in chat
- `/aiui:upload` — hands a file from the user's machine to the agent session

## License

MIT — see [LICENSE](https://github.com/byte5ai/aiui/blob/main/LICENSE).
