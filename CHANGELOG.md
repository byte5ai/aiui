# Changelog

All notable changes to this project are documented here.

## [0.3.1] — 2026-04-24

### Fixed

- **Update-Dialog UX.** The "you're on the latest version" info and the
  "update check failed" warning both used `ask()` (Yes/No), producing a
  nonsensical two-button dialog where the user had nothing to answer.
  Switched to `message()` (single OK button) for pure-info outcomes;
  `ask()` stays on the actual "install update?" prompt where a decision
  is needed.

## [0.3.0] — 2026-04-24

### Added

- **Unified native MCP server.** aiui.app now ships a full-featured MCP
  server as native Rust code inside the app bundle — confirm, ask, form,
  aiui_health, plus the new `update` and `version` tools and three
  prompts (`widgets`, `update`, `version`). Claude Code points directly
  at the app binary with `--mcp-stdio`, eliminating the `uv`/`uvx`/Python
  dependency from the onboarding path. Drag DMG → Applications → Launch
  is the whole install now.
- **`/aiui:update` slash-command.** Agent calls the `update` tool, aiui
  checks the release feed, installs any available update silently, and
  reports `{updated, current, available}` back to the agent *before*
  scheduling its own relaunch. Explicit `tokio::sleep` buffer between
  response and `app.restart()` guarantees the wire response lands before
  the process exits.
- **`/aiui:version` slash-command.** Reports the installed version,
  build info, binary path, and updater endpoint in one call.
- **`/version` HTTP endpoint** on the companion, returning structured
  build metadata (bearer-auth protected like `/render`).

### Changed

- `patch_claude_code_config` writes `{command: <aiui.app binary>, args:
  ["--mcp-stdio"]}` instead of `{command: "uvx", args: ["aiui-mcp"]}`.
- **Auto-migration** on GUI startup: existing installs from ≤ v0.2.x
  have their legacy `uvx aiui-mcp` entry in `~/.claude.json` rewritten
  to the native binary transparently. The Python `aiui-mcp` package
  stays on PyPI and remains the path of choice for remote SSH hosts
  where aiui.app isn't installed locally.
- README install section reduced from a three-step "brew + download +
  drag" to a single "download + drag + launch". The `uv` FAQ entry now
  answers "no, you don't need it."

## [0.2.8] — 2026-04-24

### Added

- **`success: true` on form actions** — green buttons for positive-outcome
  verbs ("Approve", "Publish", "Accept"). Documented in the skill and in the
  `form` tool docstring. The CSS class, TypeScript type, and skill docs
  existed in neither repo before, so agents that guessed `success: true`
  (by analogy to `destructive`) got silently-ignored styling.

### Fixed

- **Update flow no longer requires a Claude Desktop restart.** Previously,
  installing a new aiui version left the old `aiui --mcp-stdio` children
  running under Claude Desktop — bound to the old binary and, for installs
  from ≤ v0.2.5, without the auto-resurrect loop. The new GUI couldn't be
  reached until Claude Desktop was restarted. v0.2.8 sweeps those stale
  children on every GUI startup (SIGTERM against any `aiui --mcp-stdio`
  process whose executable path differs from the current one), so Claude
  Desktop respawns them against the fresh binary automatically.

## [0.2.7] — 2026-04-24

### Changed

- **Close + quit now terminate the app** (as any macOS user would
  expect). The prevent-close / prevent-exit machinery from 0.2.5 is
  gone. The auto-resurrect loop in 0.2.6 means aiui comes back on the
  next agent call anyway, so there's no reason to keep a hidden
  process running after the user asked it to go away.

### Removed

- The "Quit" button in Settings, along with its confirmation flow and
  associated i18n strings. Redundant now that red X already terminates.

## [0.2.6] — 2026-04-24

### Added

- **Auto-resurrect.** The MCP-stdio child now loops reconnect: if the
  GUI is gone (user quit, crash) and an agent call arrives, the child
  spawns aiui back up automatically. Net effect: aiui is always
  available as long as Claude Desktop is running. No more "I quit it
  and now the agent can't reach me" paper cut.
- **Quit button in Settings** (with honest wording that the next agent
  call will relaunch aiui anyway). Useful for debugging or forcing a
  clean state; for normal use, the red X is enough.

## [0.2.5] — 2026-04-24

### Fixed

- **Scroll indicator** in dialog windows. macOS's default overlay
  scrollbars are invisible until you scroll — hiding the fact that
  there's more content below. aiui now shows a slim persistent
  scrollbar when a form overflows the window.
- **Sortable lists** now actually stay in the new order. Drag-drop was
  missing `preventDefault` + `dataTransfer` setup, so the browser
  rejected the drop and snapped items back.
- **Close + quit both hide now.** The red X and Cmd-Q both just hide
  the window and drop the app back to Accessory (no Dock icon). The
  HTTP channel to the agent stays alive so dialogs keep working. Only
  the lifetime watchdog (60s after the last MCP child exits) really
  terminates aiui — matches users' mental model of "close" and fixes
  the surprise of accidentally killing the companion via Cmd-Q.

## [0.2.4] — 2026-04-24

### Fixed

- **App icon**: replaced the brand PNG with the alpha-free macOS variant.
  macOS can now squircle-clip the canvas the way every other app icon
  looks in Launchpad/Dock — no more dark border.

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
