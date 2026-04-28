# Changelog

All notable changes to this project are documented here.

## [Unreleased]

### Added

- **Image fields now accept `http(s)://` URLs.** A new `imageresolve`
  pass on the companion fetches any `http(s)://` value found in
  `src` / `thumbnail` properties on the user's Mac, encodes the bytes
  as a `data:` URL, and inlines the result before the WebView ever
  sees the spec. The CSP stays strict (`img-src 'self' data: …`) and
  agents no longer have to base64-encode images by hand. 5-second
  timeout, 10 MB cap, parallel fetch for grids; on failure the
  original URL is left in place (broken image, logged as
  `imageresolve: fetch failed for …`).

### Documentation

- **Skill catalog clarifies image source formats.** Adds a dedicated
  "Image sources (`src` / `thumbnail`)" section explaining the two
  accepted formats (`data:` URLs, `http(s)://` URLs) and the common
  footguns (plain file paths silently broken; markdown image links
  not auto-resolved). The previous "data: URL or path" wording was
  misleading — local paths never worked. Also extends the `form`
  tool's MCP description with a one-line image-source hint, so
  agents that haven't run `/aiui:teach` still get the rule.

## [0.4.20] — 2026-04-27

### Fixed

- **Reachability probe doesn't try to launch the aiui-mcp server
  anymore.** v0.4.19's diagnostic output revealed the actual macmini
  bug: `uvx aiui-mcp --help` was meant as a "does the package resolve
  from PyPI" probe, but aiui-mcp ignores `--help` and starts the full
  MCP server which then waits for stdin. Bash hangs on the subprocess,
  ssh eventually truncates output, the script never reaches STAGE:OK.
  Probe now uses `uvx --version` instead — idempotent, no server
  side-effects, tells us uvx is reachable. The "does aiui-mcp resolve
  from PyPI" question is deferred to first-tool-call time, where any
  failure shows up as a structured Claude error with full stderr.

## [0.4.19] — 2026-04-27

### Fixed

- **Reachability probe diagnostics rewritten end-to-end.** v0.4.18 still
  produced `(exit 0)` with empty output for the macmini test case;
  Codex code review identified the cwd-relative `2>uvx_err` redirect
  as the most likely silent-abort cause. Hardenings, in order of how
  much each one mattered:
  - Probe emits `STAGE:STARTED` as the very first line, before `set +e`
    even runs. If this marker is missing, the script never made it
    past line one and the failure is upstream of our logic.
  - Temp-file for `uvx aiui-mcp --help` stderr now goes through
    `mktemp` instead of cwd. ssh-login may land in a directory that's
    not writable, in which case `2>uvx_err` would silently abort the
    script before any STAGE marker gets emitted.
  - SSH invocation is now `ssh -T ... /bin/bash --login -s --`:
    absolute bash path (no PATH dependency before bash sets up its
    env), explicit `--login` long form, `-s` for stdin, `--`
    end-of-options sentinel, `-T` to disable PTY allocation
    explicitly.
  - Rust takes `child.stdin` out and drops it explicitly after
    writing, so bash on the remote sees EOF immediately. Previous
    code used `as_mut()` which kept the pipe open through
    wait_with_output and could produce hangs / empty output in some
    runtime configurations.
  - Catchall error path now shows BOTH stdout and stderr (truncated to
    1500 chars each), with separate branches for "script never reached
    the remote" (no STAGE:STARTED) vs "script ran but no STAGE:OK".

## [0.4.18] — 2026-04-27

### Fixed

- **Reachability probe actually runs the script.** v0.4.17 changed the
  invocation to pipe the script via SSH stdin and switched to `bash -l`,
  but missed the `-s` flag — without it, bash doesn't read the script
  from stdin, starts interactively (or hangs until TTY timeout), exits
  0 with empty output. User saw "Pre-Flight-Check schlug fehl
  (exit 0)" with no diagnostic. Now `bash -ls` (login + read-from-stdin),
  matching the existing `python3 -` pattern in `run_remote_python`.

## [0.4.17] — 2026-04-27

### Fixed

- **Reachability probe finds `uvx` even when it's installed via Homebrew.**
  Tester's `customer@macmini` had `uv` installed at `/opt/homebrew/bin/uvx`,
  but Homebrew's `brew shellenv` only writes to `~/.zprofile`, not the
  bash login profile that SSH uses. Result: `command -v uvx` came back
  empty in the probe's bash login shell, even though uvx was right
  there. Probe now checks four well-known install locations
  (`/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin`, `~/.cargo/bin`)
  in addition to `command -v` lookup.
- **Probe script no longer mangled by SSH word-splitting.** Previously
  passed as a multi-line `bash -lc <script>` argv, the script got
  word-split on the remote shell — producing `bash: -c: option requires
  an argument` mixed into the diagnostic output. Probe is now piped via
  stdin (same pattern as `run_remote_python`) so the script reaches
  bash unmangled.
- **Remote `~/.claude.json` records the absolute uvx path.** Previously
  written as `{"command": "uvx", "args": ["aiui-mcp"]}`, which depends
  on Claude Code's process PATH at spawn time including a directory
  with uvx — fragile (Claude launched from Finder via launchd may have
  a minimal PATH). The reachability probe now returns the absolute
  uvx path it discovered, and `patch_claude_code_config_remote` embeds
  it directly: `{"command": "/opt/homebrew/bin/uvx", "args": [...]}`.
  No more PATH-dependence.

## [0.4.16] — 2026-04-27

### Fixed

- **Add-remote failures land where the user can see them.** Previously
  on failure the input field was cleared and the only feedback was a
  log entry buried below the fold of the Settings window. Now the
  input keeps its content (so the user can fix and retry), and the
  first failing step is shown inline as a red banner directly under
  the input with the full error detail. Issue surfaced 2026-04-27 by
  tester re-adding their remotes after a fresh install — they thought
  the host had been added, only noticed the failure when scrolling
  down later.
- **Reachability check actually tells you what's wrong.** Old probe
  silenced both stdout and stderr of every inner command, so failure
  details surfaced as "SSH stderr: (empty)" — useless. New probe is a
  multi-stage script that emits tagged STAGE markers and forwards the
  inner stderr (PATH dump on `uvx`-not-found, full error from `uvx
  aiui-mcp --help` on resolve failures). Failure details now name the
  exact step and include the remote's diagnostic output verbatim.
- **"Problem melden"-Button öffnet jetzt tatsächlich GitHub.** The
  click did nothing because `window.open()` is blocked in Tauri's
  WebView for security. Replaced with a Tauri command `open_url` that
  passes the URL to macOS `open`. Defensive: only allows http(s)
  schemes. Surfaced 2026-04-27 by tester clicking the button for the
  first time.
- **Sortable list items no longer snap back after drag-drop.** Tauri
  windows have file-drop handling enabled by default, which silently
  swallows HTML5 drag-drop events inside the page — `ondrop` never
  fired, the list-item visually returned to its original position.
  `dragDropEnabled: false` on the main window lets HTML5 DnD events
  through. Surfaced 2026-04-27 in tester's demo-prompt run.

## [0.4.15] — 2026-04-27

### Changed

- **Welcome banner restructured as a 3-step stack.** Tester feedback on
  v0.4.14: "viel zu scrollen … vielleicht wäre ein Wizard". The four
  sections of the old banner (status checks + restart action + demo
  block + dismiss) reshaped into three numbered, visually-distinct
  steps:
  1. **Setup geprüft** — collapses to one line ("Alles bereit ✓") when
     all four health checks pass; auto-expands with the failing checks
     called out when something needs attention.
  2. **Claude Desktop neu starten** — primary blue CTA, imperative
     copy. No longer reads as optional. This is the must-do step after
     fresh install.
  3. **Demo-Prompt in Claude einfügen** — primary blue copy button,
     short tail explaining what happens after pasting.
- Footer dismiss is now a quiet text link ("Fertig — nicht wieder
  anzeigen") so it doesn't compete with the two primary actions for
  attention.

## [0.4.14] — 2026-04-27

### Fixed

- **Literal markdown leaked into the UI.** Two i18n strings used
  Markdown-style emphasis (`*Quit aiui*` in the uninstall modal,
  `**not**` in the welcome body) but the modal/banner renders as plain
  text, so the asterisks showed up verbatim. Replaced with plain prose
  that lets the visually-prominent buttons / layout carry the emphasis
  on their own.

## [0.4.13] — 2026-04-27

Final polish ahead of promotion. Bundles the bugs found while the
tester actually used the demo prompt end-to-end.

### Fixed

- **Sortable list field comes back empty when items were plain strings.**
  Form field `kind: "list"` documents `items: [{label, value}, …]`, but
  agents commonly emit plain string items (`["Tokyo", "Delhi", …]`)
  because the tool's input schema doesn't surface ListItem's shape.
  Result: list rendered empty, submit returned `""`. Form widget now
  normalizes string items to `{label, value}` so both shapes work.
- **Checkbox label rendered twice.** The generic `<label>` rendered
  above every form field, plus the inline label next to the checkbox
  itself. Outer label is now suppressed for `kind: "checkbox"`.
- **Form-tool description now spells out the most error-prone field
  shape.** A concrete sortable-list example is included in the tool
  description so the agent has a working spec to mimic instead of
  guessing.

### Changed

- **Settings header carries a quiet "runs in the background" line.** The
  tester closed aiui with the window X expecting the app to stop, then
  was confused when it auto-resurrected. Single dim line under the
  green status row makes the daemon-like behavior explicit, in plain
  language ("Läuft im Hintergrund — Fenster jederzeit schließbar"), no
  protocol vocabulary.
- **Demo prompt updated.** Drops the awkward "Tab 3 Aktion" framing
  (action buttons live at the form's bottom row, not inside a tab) and
  spells out the sortable-list spec inline so the agent can copy it
  verbatim.

## [0.4.12] — 2026-04-27

### Fixed

- **Cold-start race after closing aiui via the window X.** Closing the
  GUI by the red X exits the process — fine — and `mcp_attach`'s
  auto-resurrect path then re-spawns the GUI on the next tool call.
  But the GUI takes a beat to bind port 7777, and Claude's tool call
  was hitting that port before the bind landed, getting connection-
  refused, and reporting "aiui not reachable" even though aiui was in
  fact coming up half a second later. mcp-stdio now polls `/ping` for
  up to 8 s on every tool call before dispatching — masks the cold-
  start window invisibly. If the HTTP endpoint really doesn't come up
  in time (e.g. the user is on a remote dev host whose SSH-reverse-
  tunnel is genuinely down), the response is a clear, actionable
  message instead of a raw connection error.

## [0.4.11] — 2026-04-27

### Changed

- **Demo prompt rewritten as a real show-off.** Old prompt produced a
  form with four near-identical select boxes — boring, didn't sell what
  aiui actually does. New prompt asks the agent to build a tabbed form
  exercising sortable list (sort 8 random world cities by population),
  selectbox, checkboxes, slider, color picker, and three action buttons
  with destructive/success variants. Agent makes up the concrete content
  itself and follows the session language. Triggered as natural prose
  rather than via slash command — the wow effect is bigger when Claude
  reaches for the tools on its own.
- **Welcome banner trimmed.** The demo prompt no longer renders as a
  read-only textarea; just a one-line description of what the demo
  contains plus a "Copy demo prompt" button. Cleaner, faster scan.
- **Scope hint added.** New users were trying the demo in fresh Claude
  Desktop chats and wondering why nothing happened. Most likely cause:
  Claude Desktop was already running when aiui got installed, so it
  hadn't picked up the new MCP server. Banner now points directly at
  the existing "Restart Claude Desktop" button for that case.

## [0.4.10] — 2026-04-27

Release-grade pass. Codex-assisted code review (`docs/reviews/v0.4.10-codex-review.md`)
plus structural fixes addressing several real defects that survived the
v0.4.5 → v0.4.9 reactive cycle.

### Fixed

- **No more phantom GUIs on remote hosts.** `mcp_attach`'s auto-resurrect
  loop now suppresses `open -a aiui --args --auto` when running in a
  non-interactive session (SSH detected via `SSH_CONNECTION` /
  `SSH_CLIENT` / `SSH_TTY`). On macmini and other dev hosts the MCP-stdio
  child trusts the SSH-reverse-tunnel back to the user's machine instead
  of spawning a window nobody can see. (#80)
- **HTTP self-probe verifies the server is actually aiui.** Previous
  v0.4.9 probe was a naked `TcpStream::connect` that any port-7777
  squatter (sshd-session, unrelated process) would answer with TCP-SYN,
  silently lying "healthy". Probe now hits `/probe` with the bearer
  token and verifies the `aiui: true` marker. Anything else reads as
  down. (#74, #77 revised)
- **Bundle drift detector in release pipeline.** `scripts/release.sh`
  now sanity-checks the `version` field across `Cargo.toml`,
  `tauri.conf.json`, and the bundled `Info.plist` after `tauri build`.
  Mismatch aborts the release before any tag/upload. (#82)
- **Stale dialog state no longer leaks between consecutive renders.**
  Dialog widgets (Confirm/Ask/Form) get a `{#key dialog.id}` wrapper so
  Svelte unmounts the previous instance and remounts a fresh one for
  every new render call. Previously, two consecutive same-kind dialogs
  could carry over field values from the first into the second —
  silently corrupting answers sent back to the caller.
- **XSS surface closed.** Markdown fields in `form` are now piped
  through DOMPurify before `{@html}`, with `<script>`, `<iframe>`,
  `<form>`, `<input>`, `<button>` and inline event handlers stripped. A
  meaningful CSP (`default-src 'self'; script-src 'self'; …`) replaces
  the previous `csp: null`.
- **Linux dev-host setup verifies `uvx aiui-mcp` reachability.**
  `add_remote` does an SSH probe (`bash -lc 'uvx aiui-mcp --help'`)
  before persisting a host. Hosts without `uv` installed fail fast with
  a pointer to the install instructions instead of producing a broken
  `~/.claude.json` entry. (#81)
- **Idle-deadline survives suspend/resume.** mcp-stdio's 6 h
  no-input-exit now double-checks the wall-clock elapsed time before
  exiting; if a Tokio timer fires early after a long suspend, the loop
  rearms instead of bailing.
- **Idle-restart respects pending dialogs.** The 24h+ uptime + 10min
  quiet WebView reload no longer fires while a dialog is still
  registered — would have killed the user's open dialog mid-interaction
  and dropped the answer.
- **Cancellation reasons are now structured.** `RenderResponse` (and the
  internal `DialogResult`) carries an optional `reason` field
  (`ttl_expired`, `evicted`, `channel_dropped`) so MCP callers can
  distinguish a user cancel from a registry-side timeout/eviction.
- **Atomic writes for all user-config files.** `claude_desktop_config.json`,
  `~/.claude.json`, `~/.ssh/config`, `~/.config/aiui/remotes.json`, and
  the auth token now go through `fsutil::atomic_write` (sibling temp
  file + fsync + rename). Crash mid-write leaves either the old file or
  the new one — never a half-written corrupted destination.
- **Trace log rotation.** `/tmp/aiui-trace.log` rotates to `.log.1` once
  it crosses 4 MiB, on next process start. Prevents unbounded growth
  under long auto-resurrect / multi-mcp-stdio fan-out.
- **Release script is fail-fast.** `set -euo pipefail` at the top so a
  half-finished release (jq missing, codesign failure, plist mismatch)
  doesn't quietly continue to push tags or upload broken artifacts.

### Changed

- `is_interactive_session()` in `lifetime.rs` is the single decision
  point for "should we ever launch a GUI here?". Cross-platform from the
  start so the planned Windows port can extend without re-architecting.
- Skill description (`docs/skill.md` frontmatter) reads cleanly in the
  Claude slash-command help. No platform-specific copy in the
  description; user-facing copy throughout has been swept for
  unnecessary "macOS"/"Mac" mentions in preparation for the Windows port.

## [0.4.9] — 2026-04-27

### Fixed

- **HTTP-server liveness probe now actually works on macOS.** v0.4.8's
  fix for the stale `http_error` banner used a WebView `fetch()` to
  poll `/ping`. macOS App Transport Security blocks plaintext HTTP
  requests from WKWebView by default — including to localhost — so the
  probe always failed and the banner stayed permanently red on healthy
  servers. The probe now runs Rust-side: a quick TCP connect to
  `localhost:cfg.http_port` with 200 ms timeout, result delivered as
  `http_alive: bool` in the existing `StatusReport`. Tokio doesn't go
  through WebView's net stack, so ATS is irrelevant. Closes #77.
- **Settings footer no longer rolls under the fold.** With the welcome
  banner expanded the content exceeded the fixed 560 px window and the
  Uninstall / Updates / Report buttons disappeared below the bottom
  edge — invisible without scrolling, which macOS settings panes
  conventionally don't have. Footer is now `position: sticky; bottom: 0`
  with an opaque background, pinned to the visible viewport while
  content scrolls above. Closes #78.

## [0.4.8] — 2026-04-27

### Fixed

- **Port binding now survives restarts.** Tokio's default `TcpListener::bind`
  did not set `SO_REUSEADDR` before bind. On macOS that means after every
  aiui exit the kernel held the socket in TIME_WAIT for 30–60 s, and any
  fresh aiui starting in that window failed to bind with "Address already
  in use" — even though no real squatter existed. Combined with the
  never-reset `http_error` mutex (next bullet), users saw a permanent red
  banner from a transient race. Now we go through `socket2`, set
  `SO_REUSEADDR`, then hand the listener to Tokio. Restarts within the
  TIME_WAIT window bind cleanly. Closes #75.
- **HTTP-error banner is no longer stale.** The banner used to read
  `status.http_error`, a one-shot Rust-side string set when the initial
  bind failed and never reset. The Settings refresh now probes
  `localhost:7777/ping` directly every 2 s and shows the banner only when
  the probe actually fails. The `http_error` text from the original
  failure is still surfaced as explanatory detail when the probe is dead,
  but it doesn't keep the banner alive after the server recovers. Closes
  #74.

## [0.4.7] — 2026-04-26

### Fixed

- **Settings window popped up every second after a failed render call.**
  Root cause: the single-instance plugin's callback fired on every
  invocation (including auto-resurrect attempts via `open -a aiui --args
  --auto`) and unconditionally surfaced the Settings window. When
  `mcp_attach`'s 500 ms reconnect loop kicked in (because of any
  transient companion failure), the user saw Settings flashing forever
  until they force-quit Claude Desktop. Callback now ignores `--auto`
  entirely. Closes #71.
- **Uninstall didn't quit aiui, so the user couldn't drag aiui.app to
  the Trash.** Modal button changed from "Schließen" to "aiui beenden";
  it now invokes a new `quit_app` Tauri command that SIGTERMs every
  `aiui --mcp-stdio` child first (so they can't resurrect the GUI via
  `mcp_attach`), pauses 300 ms for the kill to land, then `app.exit(0)`.
  Closes #72.

### Changed

- **"Test-Dialog jetzt"-Button removed.** It looped back through aiui's
  own `/render` endpoint and proved nothing the user couldn't already
  see — this Settings window itself is rendered by the same WebView. It
  was honest noise. Replaced with a "Erste Schritte in Claude" block in
  the welcome banner that shows the `/aiui:test-dialog` and `/aiui:teach`
  slash commands plus a copyable demo prompt for any Claude chat. Closes
  #70.

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
