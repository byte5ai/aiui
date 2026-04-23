# aiui

A generic UI channel for Claude Code sessions — local or remote. Lets the agent
render native dialogs on the user's Mac and get structured responses back,
without polluting the project with ad-hoc web dashboards.

## Why

Claude Code has exactly one built-in interactive widget: `AskUserQuestion`.
Everything else — a form, a list with preview, a confirmation with destructive
styling, a sortable pick-and-order — has to be worked around with chat prompts.
Worst of all, when the agent runs on a remote host via SSH, it can't reach any
UI on the user's Mac at all.

**aiui** plugs that gap with one MCP server (`server.py`) and one Tauri
companion app. The agent calls a tool like `aiui.form(...)`, the companion
renders a native window on the Mac, the user interacts, and the structured
response flows back. Works identically for local and remote setups.

## Architecture

```
Mac (user)                                      Remote host (optional)
┌──────────────────────────┐                    ┌──────────────────────────┐
│ Claude Desktop           │                    │ Claude Code CLI          │
│   └─ aiui-local MCP      │                    │   └─ aiui MCP server.py  │
│      (stdio)             │                    │      (stdio)             │
│         └─ Unix socket   │                    │                          │
│            └─ aiui.app ◄─┼── SSH -R tunnel ───┤                          │
│               HTTP :7777 │                    │ HTTP POST 127.0.0.1:7777 │
│               WKWebView  │                    │                          │
└──────────────────────────┘                    └──────────────────────────┘
```

For purely local use, the SSH tunnel is skipped — the MCP server posts
directly to `127.0.0.1:7777`.

## Install (macOS, Apple Silicon)

1. Download the latest `aiui.app` release and drop it into `/Applications/`
   (release bundles are unsigned for now — you need to clear the quarantine
   flag once):
   ```sh
   xattr -dr com.apple.quarantine /Applications/aiui.app
   ```
2. Launch it once. On first start the app:
   - generates a local auth token at `~/.config/aiui/token`
   - patches `~/Library/Application Support/Claude/claude_desktop_config.json`
     so Claude Desktop will auto-spawn the MCP child
   - shows the settings window where you can (optionally) register remote
     hosts
3. Restart Claude Desktop. From now on the app lives in the background: it
   comes up automatically when Claude Desktop starts, terminates itself 60
   seconds after Claude Desktop quits, and never shows a dock icon unless
   you launch it manually.

## Use from a Claude Code project

Drop this into `.mcp.json` in your project root:

```json
{
  "mcpServers": {
    "aiui": {
      "command": "/opt/homebrew/bin/uv",
      "args": ["run", "/path/to/aiui/server.py", "--stdio"]
    }
  }
}
```

Then in the session:

```
aiui_health       # sanity check
aiui_confirm(title="Go ahead?", message="Will deploy to prod.")
aiui_form(title="Feature brief", fields=[...], actions=[...])
```

See [`docs/widgets.md`](docs/widgets.md) for the full widget catalog with
decision rules, patterns, and anti-patterns.

## Remote setup

If you run Claude Code via SSH on a remote host, open the settings window
in the companion and add the remote's SSH alias (e.g. `cw@macmini`). The
companion will:

- add a `RemoteForward 7777 localhost:7777` to `~/.ssh/config` for that host
- `scp` the auth token to `~/.config/aiui/token` on the remote
- maintain a persistent `ssh -NTR`-tunnel with automatic reconnect

Repeat for every remote you work against. The companion keeps one tunnel per
remote alive while it runs.

## Widgets (v1.1)

| Tool | Purpose |
|---|---|
| `aiui.confirm` | Yes/no with optional destructive styling |
| `aiui.ask` | Single- or multi-choice with per-option descriptions and free-text fallback |
| `aiui.form` | Composite window: mix `text`, `password`, `number`, `select`, `checkbox`, `slider`, `date`, `static_text`, `list` fields with multiple action buttons |
| `aiui.aiui_health` | Reachability + token check |

The `list` field covers static info lists, checkbox lists, single-choice
radios, sortable drag-lists, and pick-and-order, all via three flags.

## Build from source

Prerequisites: Rust (stable), Node.js ≥ 20, Xcode command-line tools.

```sh
cd companion
npm install
npx tauri build --target aarch64-apple-darwin
```

Output: `companion/src-tauri/target/aarch64-apple-darwin/release/bundle/macos/aiui.app`

## License

MIT — see [LICENSE](LICENSE).
