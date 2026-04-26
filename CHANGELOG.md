# Changelog

All notable changes to this project are documented here.

## [0.4.6] — 2026-04-26

### Fixed

- **`/aiui:test-dialog` returned "Unknown command" on Claude Desktop.**
  Root cause: `claude_desktop_config.json` was written under the key
  `aiui-local` while `~/.claude.json` used `aiui`. Slash commands on
  Claude Desktop would have needed `/aiui-local:test-dialog`. Both
  configs now use `aiui`; legacy `aiui-local` entry is removed on every
  patch (idempotent for fresh installs, healing for upgrades).

### Changed

- **Welcome banner is a live setup health-check.** Replaces the static
  "everything ready" copy with four real checks read at refresh time:
  Claude Desktop config registered, Claude Code config registered,
  skill installed, HTTP server up. Each row shows ✓ / miss in real time.
  Banner now also tells the user explicitly that aiui does **not**
  appear in Claude Desktop's Connectors list — that's only for cloud
  services.
- **"Skill installieren" button replaced with status row.** The old
  button suggested optionality where there isn't any (the skill is
  mandatory and auto-installed every GUI launch). Now a quiet "Skill
  installiert ✓" row by default; only if the file is missing does a
  red row + "Skill reparieren" button appear.

### Added

- **"Test-Dialog jetzt" button.** Pops a small confirm dialog through
  the local aiui server, end-to-end, without going through Claude.
  Verifies the wiring strecke independently.
- **"Claude Desktop (neu) starten" button.** Quits + relaunches Claude
  Desktop so it re-reads the current `claude_desktop_config.json`
  entry. Label switches between Start / Restart depending on whether
  Claude is running.
- **Uninstall completion modal.** After Uninstall removes configs,
  tokens, and the skill, a modal explains that and points the user at
  the Finder for the actual `.app` removal — running app self-deleting
  is fragile, and "Uninstall" is honest about being a configuration
  cleanup, not self-destruct.

## [0.4.5] — 2026-04-26

### Removed (breaking, pre-1.0)

- **`/aiui:widgets` slash-command.** Replaced by `/aiui:teach` (added
  in 0.4.4 as an alias). The `widgets` name was a leftover from the
  era when the command listed widget names; today it briefs the agent
  on the full design rules. Cleaning up before a wider audience picks
  up the old name from old blog posts. Same goes for the skill
  frontmatter `name`: `aiui widgets` → `aiui`.

### Notes

- Anyone who picked up the alias in v0.4.4 only needs to type
  `/aiui:teach` from now on. Same content, same effect.
- `aiui-mcp` PyPI 0.4.1 → 0.4.2 for parity.

## [0.4.4] — 2026-04-26

### Added

- **`initialize.instructions` injection.** The MCP `initialize` response
  now carries a top-level `instructions` string that Claude Code (and
  Claude Desktop) feed to the agent before the first turn. Short
  imperative brief telling the agent to default to dialogs for yes/no,
  pick-one-of-N, and multi-input prompts. Replaces the previous purely
  passive trigger model where the agent had to *deduce* aiui-relevance
  from the skill description.
- **`/aiui:teach` slash-command** as a discoverable alias for
  `/aiui:widgets`. Same content (full widget catalog), more telling
  name. The old `/aiui:widgets` keeps working.

### Changed

- **Tool descriptions on imperative voice** ("Before writing a yes/no
  question into chat, call this tool instead.") instead of "USE WHEN…".
  Same content, but framed as a directive — gives the agent a stronger
  pull out of the chat-first default.
- **Skill frontmatter description** rewritten to start with the trigger
  ("Before writing a yes/no question, a numbered option list, or a
  multi-question request into the chat, …"). Same goal: prime the
  agent to *use* aiui rather than *consider* aiui.
- **`aiui-mcp` PyPI 0.4.0 → 0.4.1** for parity (instructions field,
  imperative docstrings, `/aiui:teach` alias).

## [0.4.3] — 2026-04-26

### Changed

- **Settings header uses the app icon, not the wordmark.** The wide
  white-card "aiui" logo at the top of the Settings window has been
  replaced with the square app icon (32 px, rounded 7 px). Cleaner
  silhouette, identical visual identity to the Dock/Launchpad/Finder
  presence, and the title-area is no longer dominated by a graphic
  that didn't add information.

## [0.4.2] — 2026-04-25

Pre-launch hardening pass: external code review surfaced six substantive
defects in the SSH-tunnel and remote-setup paths. All fixed.

### Fixed

- **Remote `~/.claude.json` patching/removal silently no-op'd** (#51,
  blocker). The previous implementation tried to ship the python script
  through `python3 -c "$1"` over ssh; the remote login shell expanded
  `$1` to empty *before* python ran, producing a no-op + `print("ok")`
  the Rust side trusted. `add_remote` claimed success while the remote
  config was untouched. New implementation pipes the script through
  ssh's stdin to `python3 -`, and verifies the `"ok"` marker before
  reporting success.
- **SSH option injection via `host_alias`** (#52, security/high). User
  input from "Add Remote" flowed unvalidated into `ssh`/`scp` argv;
  values starting with `-` were interpreted as ssh options
  (e.g. `-oProxyCommand=…`). Now validated at the API boundary
  (`is_valid_host_alias`), with defense-in-depth `--` end-of-options
  markers everywhere ssh/scp invokes a host. 15 unit tests cover the
  validator.
- **Shared-forward detection trusted unauthenticated `/ping`** (#53,
  security/high). A squatter on remote port 7777 could mask a
  port-takeover by answering "pong", flipping the tunnel to
  `ConnectedShared` (green). Replaced with a token-authenticated
  `/probe` endpoint; the remote-side curl reads
  `~/.config/aiui/token` and sends it as `Authorization: Bearer …`.
  Only an aiui that authenticates against the same token is accepted
  as a shared owner.
- **`add_remote` was not transactional** (#54). Half-failed setup
  steps still appended the host to `remotes.json` and started the
  tunnel manager retrying forever. Now: token-push and config-patch
  are blocking (must succeed before persistence + tunnel start). Skill
  install stays non-blocking (warn-only). The user sees per-step
  results either way.
- **HTTP bind failure was logged-and-swallowed** (#55). If port 7777
  was already held when aiui started, the GUI looked alive but every
  request failed silently. New `http_error` field in the `status`
  payload + a red banner in Settings ("aiui can't accept requests")
  that points at the squatter and tells the user how to find it
  (`lsof -nP -iTCP:7777`).
- **`/health` reported ready exactly at the dialog hard-cap** (#56).
  At `len() == HARD_CAP` the next `register()` evicts an in-flight
  dialog while `/health` still claims ready. Tightened the gate to
  `< HARD_CAP` so readiness leads eviction.

### Notes

- `aiui-mcp` PyPI version unchanged (no Python-side changes in this
  release).
- The Codex review's Windows-port readiness finding (low severity, #57)
  is tracked but not addressed here.

## [0.4.1] — 2026-04-25

### Added

- **Welcome banner on first run.** The Settings window now shows an
  onboarding card on the first launch (and on every subsequent launch
  until the user clicks "Got it"). Tells the user aiui is set up,
  points at `/aiui:test-dialog` and `/aiui:widgets` for a 30-second
  smoke test, and prompts SSH users to add their dev host. Replaces
  the previous behaviour where first-run state was marked done as
  soon as the window opened — users who closed the window without
  reading anything had no second chance.

### Changed

- `status` Tauri command returns a new `welcome_pending` boolean.
- New `dismiss_welcome` command marks the banner dismissed (writes
  `~/.config/aiui/first_run_done`).
- The auto-mark-done call moved out of `setup` into the
  user-controlled dismiss path.

## [0.4.0] — 2026-04-25

### Added — new form fields

- **`datetime`** — gap-filler between `date` and `date_range` for cron,
  scheduling, reminders. Native `<input type="datetime-local">`.
- **`markdown`** — read-only Markdown block as inline context for
  following input fields ("here's the diff I generated, now decide…").
  Skill clarifies: not a standalone display tool.
- **`image`** — single read-only image preview (`src`: data: URL or
  path). For visual sign-off on agent-generated charts/screenshots/
  diagrams before the next decision.
- **`image_grid`** — n × m image picker, optional multi-select. For
  "pick one of these N generated logos / thumbnails / asset variants".
- **`table`** — column-aware row triage with per-column header sort,
  multi-select, and structured `{rows: [{value, values}], columns:
  [{key, label, align?}]}` spec. For 30-branch / 50-search-result
  triage flows that `list` couldn't do.
- **`list` thumbnails** — existing `list` field gets an optional
  `thumbnail` per item (data: URL or path). Shotlists, mood boards,
  carousel-slide ordering with the visual anchor that matters.

### Added — tabbed forms

`form` now accepts `tabs=[{label, fields: [...]}]` instead of the flat
`fields=…`. One submit covers all tabs; validation jumps to the first
invalid tab automatically. Tabs are display structure, not a wizard —
no per-tab confirmation, no per-tab actions, all values in one
response.

### Added — three new slash-commands

- **`/aiui:health`** — one-line aiui health check (WebView responsive,
  no dialog backlog, no child-process flood).
- **`/aiui:test-dialog`** — pops a tiny demo dialog so the user can
  verify aiui is wired up end to end.
- **`/aiui:remotes`** — lists registered aiui remotes in chat (same
  set the Settings window shows).

All three available in both the Rust MCP (aiui.app) and the Python MCP
(aiui-mcp on PyPI) for parity across local and remote sessions.

### Polish

- **Dark-mode palette warmer.** Replaced the neutral-zinc tones with a
  warmer brown-tinted dark to match macOS Sonoma feel without
  introducing a colour cast on text. Light-mode palette unchanged
  (already tested in marketing screenshots).
- **Buttons get a subtle macOS gradient and a stronger hover state.**
  Neutral buttons now have a top-down gradient (AppKit push-button
  feel) and on hover pick up an accent-tinted shadow + border, so the
  hover-vs-not distinction is unmistakable. Active state scales 0.97
  (was 0.98) for snappier feedback.
- **Settings window default height 480 px** (was 560). Idle Settings
  no longer sits in a sea of empty space when no remotes are
  registered. `maxHeight` raised to 640 for forms that need more
  vertical room. `minHeight` 380.
- **App-header padding** tightened so the aiui logo doesn't crowd the
  title-area boundary.

### Notes

- `aiui-mcp` Python package bumped 0.3.1 → 0.4.0 to match the
  companion. Adds `tabs`, all new field kinds, and the three new
  slash-commands.

## [0.3.3] — 2026-04-24

### Fixed

- **Long-running installs now actually auto-update.** The updater only
  ran at app startup (`onMount`), so an aiui instance that had been up
  for hours or days silently stayed on its install version while new
  releases shipped. Added a recurring silent check every 6 h on top of
  the existing startup check. The call stays silent unless an update
  is actually available.

## [0.3.2] — 2026-04-24

### Added

- **Shared-forward detection.** A stale `sshd-sess` (commonly an earlier
  aiui session whose parent died but whose child forward kept running)
  can hold port 7777 on a remote indefinitely. aiui's own `ssh -NTR`
  then fails with `ExitOnForwardFailure` (exit 255) — but the forward
  actually works, we just don't own it. Previously the UI showed a
  hard red "Failed: ssh exit code 255" in that case, which was
  misleading. v0.3.2 probes the remote after each ssh failure
  (`ssh host curl -f http://localhost:7777/ping`); if a `pong` comes
  back, the tunnel flips to a new `ConnectedShared` state — green,
  labelled "connected (shared forward)" / "verbunden (geteilter
  Forward)" — and polls every 30 s instead of spamming `-NTR` retries.
  When the external owner dies, aiui drops back into the normal retry
  loop. Closes the most-common "ssh exit 255" support issue.

### Changed

- **Python `aiui-mcp` package (v0.3.1 on PyPI) gains feature-parity
  with the native Rust MCP.** Adds `version` and `update` tools plus
  `/aiui:version` and `/aiui:update` prompts. Prompt texts are
  byte-identical to the Rust implementation so agent behaviour is
  uniform regardless of whether the MCP lives in the app bundle or on
  PyPI. Relevant for remote SSH hosts without aiui.app installed
  locally.

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
