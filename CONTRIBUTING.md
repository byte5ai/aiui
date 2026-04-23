# Contributing to aiui

Thanks for reading this. aiui is a small project; contributions are
welcome as issues, discussions, or pull requests.

## Quick links

- [Open issues](https://github.com/byte5ai/aiui/issues)
- [Widget catalog](docs/skill.md) (how aiui is meant to be used)
- [Changelog](CHANGELOG.md)

## Repository layout

```
aiui/
├── companion/                Tauri companion (Rust + Svelte 5)
│   ├── src-tauri/            Rust backend, HTTP + lifetime + tunnel manager
│   └── src/                  Svelte frontend — settings window + dialog widgets
├── python/                   aiui-mcp PyPI package (FastMCP server)
│   └── src/aiui_mcp/
├── docs/
│   └── skill.md              Agent-facing widget catalog (shipped into
│                             ~/.claude/skills/aiui/)
├── scripts/
│   └── release.sh            Local sign + notarize + updater feed pipeline
├── assets/                   Brand assets (icon, logo, dmg background)
├── server.py                 Legacy standalone script (for `uv run`) —
│                             mirrors python/src/aiui_mcp/server.py
└── CHANGELOG.md
```

## Building locally

Prerequisites: Rust (stable), Node.js ≥ 20, Xcode command-line tools,
[uv](https://docs.astral.sh/uv/).

```sh
cd companion
npm install
npx tauri build --target aarch64-apple-darwin
```

Output: `companion/src-tauri/target/aarch64-apple-darwin/release/bundle/macos/aiui.app`

For the Python side:

```sh
cd python
uv build    # produces dist/aiui_mcp-*.whl + .tar.gz
```

## Signing / notarising / releasing

The release pipeline lives in `scripts/release.sh` and is tuned for the
byte5 Developer ID. If you fork aiui, copy `.env.release.example` (if
present) or read the script; the required environment variables are:

- `APPLE_SIGNING_IDENTITY`, `NOTARY_PROFILE`,
  `BUILD_KEYCHAIN`, `BUILD_KEYCHAIN_PASS_FILE` — Apple codesign + notary
- `TAURI_SIGNING_PRIVATE_KEY_PATH` — Ed25519 key for the updater feed

Running `scripts/release.sh 0.2.3` builds, signs, notarizes, creates a
DMG + updater artifacts + `latest.json`, tags, pushes, and creates the
GitHub release via `gh`.

## Issues

The aiui app's Settings window has a „Report issue" button that opens a
prefilled GitHub issue with the current version + build SHA. Use it — it
saves us a round of „which version are you on?".

For bug reports, please include:

- aiui version (visible in Settings as a chip, e.g. `v0.2.0`)
- macOS version
- Whether you hit it locally or via a remote host setup
- What you did / expected / saw

## Pull requests

1. Keep changes focused. One PR, one concern.
2. If you're adding a widget, also extend `docs/skill.md` with an anti-
   pattern section — every widget needs guidance for the agent, otherwise
   it degrades into UI slop.
3. New user-facing strings: add them to `companion/src/i18n/de.json` and
   `en.json`, keyed by short stable paths.
4. Run `npm run check` in `companion/` before pushing — catches Svelte /
   TypeScript issues early.

## Design principles

The constraints that shape decisions in this project:

- **User installs nothing per project.** The MCP server is on PyPI; a
  single line in `.mcp.json` is enough for any project.
- **Agents can't make slop.** Rules live both in tool docstrings (always
  visible) and the full skill (auto-installed). Widgets constrain rather
  than expand freedom where that improves outcomes.
- **No ad-hoc web dashboards.** aiui exists to replace the pattern of the
  agent spinning up a temporary local web UI. If a feature request pulls
  in that direction, the answer is usually „more widget primitives" not
  „more escape hatches".
- **Updates are zero-friction.** Every release must flow through the
  in-app updater; no manual zip-swap dance for users.

## License

By contributing, you agree your contribution is licensed under the MIT
license (see [LICENSE](LICENSE)).
