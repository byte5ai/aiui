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
- `UV_PUBLISH_TOKEN` — PyPI API token (project-scoped to `aiui-mcp`).
  Without it, `scripts/release.sh` aborts before any work is done — that
  guard exists because shipping the Tauri side without the matching PyPI
  bump produces silent slash-command-mismatch on remote hosts.

A single run of `scripts/release.sh 0.2.3` performs the full pipeline:

1. Pre-flight checks — `.env.release` loaded, all required env present,
   versions agree across `Cargo.toml`, `tauri.conf.json`, and
   `python/pyproject.toml`.
2. Tauri companion: build, codesign, notarize, staple, DMG, updater
   bundle + signature, `latest.json`.
3. Python `aiui-mcp`: `uv build` produces wheel + sdist into
   `python/dist/`. Done before the dry-run gate so dry runs catch
   packaging breakage too.
4. Tag, push, GitHub release.
5. `uv publish` from `python/` — pushes the wheel + sdist to PyPI so
   `uvx aiui-mcp` resolves to the matching version on remote hosts.

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

- **User installs nothing per project.** aiui registers itself as a
  global MCP server in Claude Code (`~/.claude.json`) on first launch;
  the PyPI package is pulled on demand via `uvx`.
- **Agents can't make slop.** Rules live both in tool docstrings (always
  visible) and the full skill (auto-installed). Widgets constrain rather
  than expand freedom where that improves outcomes.
- **No ad-hoc web dashboards and apps.** aiui exists to replace the pattern of the
  agent spinning up a temporary local web UI. If a feature request pulls
  in that direction, the answer is usually „more widget primitives" not
  „more escape hatches".
- **Updates are zero-friction.** Every release must flow through the
  in-app updater; no manual zip-swap dance for users.

## License

By contributing, you agree your contribution is licensed under the MIT
license (see [LICENSE](LICENSE)).
