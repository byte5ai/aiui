# Changelog

All notable changes to this project are documented here.

## [0.2.3] — 2026-04-24

### Fixed

- **App icon**: reverted the auto-padding from 0.2.2 that introduced a
  dark border around the artwork in dark-background contexts (Launchpad,
  Dock). Back to the original brand asset — macOS handles the canvas on
  its own.

## [0.2.2] — 2026-04-24

### Added

- **Icon padding** to the macOS safe-area standard (~80 % active), so aiui
  lines up visually with other app icons in Launchpad and the Dock.
- **Update dialog auto-surfaces.** In Accessory (hidden-dock) mode, the
  updater now temporarily promotes the app to Regular and brings the
  window forward before showing the "update available" prompt, so the
  user actually sees it without clicking aiui first.

### Changed

- **Docs language.** `docs/skill.md` is now consistently English (mixed
  German/English examples removed). Redundant `docs/widgets.md`
  dropped — skill.md is the single source of truth for agent guidance.
- `aiui-mcp` bumped to 0.2.2 to ship the English-only skill resource.

## [0.2.1] — 2026-04-24

### Added

- **Zero-config Claude Code integration.** aiui registers itself in
  `~/.claude.json` automatically on every launch, so every Claude Code
  session sees the `aiui.*` tools without a per-project `.mcp.json`.
  `add_remote` does the same on the remote host via SSH + python3.
  `uninstall` and `remove_remote` clean both sides up.

### Changed

- README no longer asks for a `.mcp.json` snippet — install = download +
  launch + restart Claude Desktop, nothing else.
- Password-field wording in README and docs: masks while typing, but the
  value returns as plaintext to the agent. Honest about the scope.

## [0.2.0] — 2026-04-24

First public release.

### Added

- **Tier-2 widgets** for composite `form` dialogs:
  - `color` — native color picker
  - `date_range` — from/to date span
  - `tree` — hierarchical selector with expand/collapse and optional
    multi-select (single or multi, no sortable variant)
- **Skill distribution** in three layers:
  - Tool docstrings carry concise dialog-design rules so every Claude Code
    session sees them on `tools/list` without any installation.
  - `/aiui:widgets` MCP prompt returns the full widget catalog on demand.
  - Auto-installation of `SKILL.md` into `~/.claude/skills/aiui/` on the
    local Mac and via `scp` to every registered remote host. A „Install
    skill" button in Settings re-installs on demand.
- **Branded macOS app** — new icon, logo in settings header, warm dark mode
  palette, gradient primary buttons, subtle entrance animation.
- **Branded DMG** — drag-to-Applications with background image, built via
  `appdmg` (no AppleScript dependency — works in headless CI).
- **`aiui-mcp` on PyPI** — `.mcp.json` is now a single line
  `{ "command": "uvx", "args": ["aiui-mcp"] }`. Previous script-path setup
  still works for local hacking.
- Settings window: „Report issue" button, „Install skill" button, „Check
  for updates" button, live tunnel status per remote.

### Changed

- README rewritten for end users, not just developers.
- Tool docstrings rewritten with explicit anti-patterns.

### Fixed

- SSH config patch removed entirely — the tunnel manager owns the forward
  exclusively. Legacy `RemoteForward` lines are cleaned up on startup and
  when a remote is removed.

## [0.1.2] — 2026-04-23

- In-app auto-updater (`tauri-plugin-updater`): silent check on startup,
  modal prompt when a new version is live, signature-verified swap and
  relaunch.
- DMG as primary release asset (in addition to the zip), built with
  `hdiutil`.
- Release pipeline emits `latest.json` signed with an Ed25519 updater key;
  shipped as a release asset for the updater feed.

## [0.1.1] — 2026-04-23

- First Apple-signed and notarized build. No more Gatekeeper „unidentified
  developer" warnings.
- Local signing + notarization pipeline in `scripts/release.sh`, using a
  dedicated build keychain.

## [0.1.0] — 2026-04-23

- Initial public release (unsigned).
- Tauri companion app (Rust + Svelte 5) rendering native macOS dialogs.
- MCP server (FastMCP) exposing `ask`, `form`, `confirm`, `aiui_health`.
- Auto SSH reverse-tunnel manager with exponential-backoff reconnect.
- Unix-socket lifetime coupling: GUI self-exits 60 s after the last MCP
  stdio child disappears.
- Remote-zombie preflight in the MCP server.
- i18n (de, en) with auto-detect.
- Dock-icon visible only on manual launch, headless when auto-spawned.
