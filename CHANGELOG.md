# Changelog

All notable changes to this project are documented here.

## [Unreleased]

### Added

- **A system notification when an update is found.** The settings banner
  used to be the only surface for a pending update, which a headless app
  nobody opens is a poor place for. The headless check now also raises a
  native notification — once per version, not once per six-hour tick.
  English-only: there is no i18n layer in the Rust half, and adding one for
  a single string is not the right trade; the localised banner remains the
  richer surface (#188).
- **`reason` and `hint` on the `/health` response.** When `ready` is false the
  body now names the cause — `webview_unresponsive`, `dialog_registry_full` or
  `too_many_children`, in that fixed precedence — and carries a
  human-readable one-liner with the live numbers in it. Agents relay the hint
  rather than inferring a cause from a status code, and the guidance can no
  longer drift from what the companion actually measures. Additive on the
  wire: both bridges parse the body generically, so no `WIRE_VERSION` bump,
  and an un-patched bridge simply sees one fewer fatal preflight (#179).
- **`scripts/check-updater-feed.sh`** — refuses a `latest.json` that is
  missing a shipped platform, carries an empty signature or url, points an
  entry at another release's artifact, or advertises the wrong version. It
  runs in the release path before anything becomes visible, and
  `scripts/test-check-updater-feed.sh` exercises it against fixtures on
  every PR so the guard cannot quietly stop guarding (#190).
- **Tests for the host-registration write path.** The companion rewrites up
  to three of the user's own config files — `claude_desktop_config.json`,
  `~/.claude.json`, `~/.codex/config.toml` — unattended on every GUI
  launch, and deletes entries from all three on uninstall. Those files are
  not aiui's: they hold every other MCP server the user configured, and
  `~/.claude.json` holds Claude Code's whole per-project state besides. Not
  one line of that path was covered, because each function computed its own
  path from `home()`, so a test would have had to rewrite the developer's
  real config. Each one is now a thin wrapper over a path-taking core, and
  the cores are exercised against temp files: fresh-file creation, foreign
  servers and top-level keys surviving, the `aiui-local` → `aiui` and
  `uvx aiui-mcp` → native-binary migrations, idempotency (no rewrite and no
  second `.bak` on an unchanged launch), backup contents, removal touching
  only aiui, and a malformed Codex config being refused byte-for-byte.
  Wrapper signatures and their call sites are unchanged, and the bytes
  written to user configs are identical (#183).
- **Windows CI runs the Rust test suite.** The `windows-latest` matrix
  entry compiled the test binary with `--no-run` and threw it away, so no
  Rust logic had ever executed on the platform aiui most recently started
  shipping — and the per-OS config paths are exactly where Windows differs.
  The loader crash that justified the skip (#141) was closed in August; the
  skip outlived it (#183).
- **`rust-toolchain.toml`** pins the compiler aiui is built with. It is a
  pin, not a floor — `rust-version = "1.77.2"` in `Cargo.toml` stays as the
  MSRV floor, and the two say different things. Because a
  `RUSTUP_TOOLCHAIN` in the environment outranks the file's directory
  override, dropping the file in is not enough on its own: each workflow
  now asserts `rustc --version` against the pinned channel right after
  installing the toolchain, so a pin that is not actually in effect fails
  the job instead of shipping (#192).
- **`scripts/check-workflow-pins.sh`** refuses a workflow whose `uses:` is
  not a 40-char commit SHA with a readable `# vX.Y.Z` comment, a
  `version: latest` uv, or a global `@tauri-apps/cli` install. It runs as
  its own CI job, and `scripts/test-check-workflow-pins.sh` exercises it
  against fixtures of every shape that was in the tree before this
  release (#192).
- **`.github/dependabot.yml`** covering `github-actions`, `npm`
  (`/companion`) and `cargo` (`/companion/src-tauri`). Pinning by SHA
  without an update path only trades a moving-ref risk for a never-updated
  one; Dependabot rewrites the SHA and its version comment together, so a
  bump stays a reviewable one-line diff (#192).
- **A frontend test runner.** The companion had none — CI ran
  `svelte-check` and a Vite build, so every line of TypeScript was covered
  by "it compiles". `npm run test` (vitest) now runs on both CI legs, with
  the update path and de/en catalogue parity as its first suites. The
  update path is where this matters most: its failures are invisible by
  construction, which is exactly what made #197 survive so long (#197).

### Changed

- **`upload`'s `session` parameter now does something.** Both bridges
  advertised it and both discarded it; the schema promised a label the user
  would see and delivered an anonymous system panel. It is forwarded as an
  optional `POST /upload` body and titles the picker — `aiui — billing-migration`
  — so a user facing several agents can tell which one is asking. Additive in
  both directions: an old companion ignores the body, a new one tolerates a
  body-less POST (#194).
- **A dropped video or audio clip is reported to the agent.** A local clip the
  bridge could not push to the media cache left the user looking at a broken
  player while the agent believed it was on screen — the failure reached the
  trace log and nowhere else. The render still proceeds unchanged; the result
  now also carries `media_warnings` naming each path and why (#194).
- **Two code comments described an auto-install path that does not exist.**
  `checkForUpdates`'s header still documented transparent install and
  relaunch, naming a file deleted in the multi-window refactor, and
  `is_update_safe_to_install` claimed a caller it lost in v0.4.44. Both sent
  the next contributor looking for a mechanism that was removed on purpose.
  The command is kept rather than deleted — it encodes the right predicate
  if install-while-idle is ever wanted — but its doc now says plainly that
  nothing calls it, and where the install would belong if it were (#188).
- **New file `~/.config/aiui/remote-uvx.json`** holds the `uvx` path per
  registered remote. Deliberately a sidecar rather than a second field in
  `remotes.json`: widening that file would make an older build parse it as
  an empty list and silently drop the user's registered hosts on a
  downgrade. An older build ignores the sidecar and behaves exactly as it
  does today. Removed with the host, and on uninstall (#184).
- **`skip_validation: true` actions no longer commit `target` file
  writes.** They are escape hatches, and an escape hatch that writes a
  credential to disk is a trap. The new `writes_targets: true` action flag
  opts one back in. An action value the spec never declared is refused
  (fail closed). Agent-facing: documented in both `skill.md` copies and in
  the `form` tool description of both bridges. Nothing that previously
  failed now succeeds; some writes that previously fired silently now
  refuse with a reason (#177).
- **Documentation updated for a two-platform product.** README, both
  `skill.md` copies, `python/README.md`, CONTRIBUTING and the strategy
  doc still described aiui as macOS-only. The README gained per-platform
  install instructions including the expected SmartScreen warning, and
  agent-facing path guidance no longer says "the user's Mac" — an agent
  reading that could reasonably assume POSIX paths on a Windows user's
  machine.
- **`remotes.json` now lives in the same config directory as everything
  else aiui owns.** `config_dir()` resolves `%APPDATA%\aiui` on Windows and
  `~/.config/aiui` elsewhere, and the token, `first_run_done` and `gui.lock`
  all follow it — but the remotes helper built `~/.config/aiui/remotes.json`
  from the home directory whatever the OS. On Windows that made
  `%USERPROFILE%\.config\aiui\` a second state directory, holding the list
  of every dev host the user had registered, that no aiui surface, the
  README or either bridge ever named. The path now follows `config_dir()`,
  and an existing file is moved there once at startup — deliberately not
  from `load_remotes`, which runs on every 2 s status tick and in the tunnel
  loops. Repointing without the migration would have silently emptied the
  remotes list of every install since v0.10.1. No-op on macOS and Linux,
  where both paths are the same file (#196).

### Fixed

- **A routine screenshot never opened a dialog.** `/render` was the one
  body-consuming route without an explicit limit, so axum's 2 MiB default
  applied. Base64 expands ~1.37×, which put the real ceiling at ~1.5 MB of
  image bytes — a sixth of the 10 MB the docs promise — and a `confirm`
  with a 2 MB Retina screenshot died *before* the handler ran: no window,
  no trace line, and a bare `413` at the agent naming neither the image nor
  a way out. The route is now capped at 64 MB with the handler's own guard
  at 48 MB, strictly below it, so what the agent gets is a structured
  `spec_too_large` carrying the likely cause and the fix. Both bridges
  surface `detail` + `hint` instead of the status code (#178).
- **An `ask` without options and a `form` without fields opened dead
  windows.** Both are optional in the tool signatures, and `options: null`
  is literally what the Rust bridge emits when the argument is omitted —
  Svelte renders that exactly like `[]`. The user got a window with the
  question and nothing but Cancel; the agent got `{cancelled: true}` with
  no reason. An option carrying only a `description` was worse: it rendered
  as a blank button and returned `null`. All three are rejected up front
  now, with a hint pointing at `confirm` where that is the right tool
  (#178).
- **A duplicate `value` blanked the dialog or silently dropped a result.**
  Every collection surface renders through a keyed `{#each}` whose key is
  agent-supplied data, and only `compare` checked it. Svelte throws on a
  repeated key in production builds too, and with no `<svelte:boundary>` in
  the app that throw tore down the whole mount: an empty, always-on-top
  window the user could not answer and an agent call that hung until the
  two-hour TTL. Where it did not throw it corrupted quietly — `gallery`
  builds `out[item.value]`, `list`/`table` resolve rows by `.find(…)`, so
  two entries collapsed into one and the agent acted on a result that was
  missing a row it believed it had. Uniqueness is now enforced for
  `gallery` items, `list` items, `table` rows, `image_grid` images, `tree`
  nodes (recursively, so cross-branch repeats that cross-link
  `expanded`/`selected` are caught too), tab `label`s and `form` field
  `name`s. Triggers were ordinary: two assets sharing a basename, two
  `config.yaml` rows from different directories, two tabs called "Options"
  (#178).
- **On Windows the companion wrote its log output into the host's
  JSON-RPC stream.** A cold start goes Claude Desktop → `aiui --mcp-stdio`
  → no GUI yet → re-spawn the GUI, and that re-spawn used a plain
  `Command::spawn()`, whose stdio defaults to *inherit*. The new GUI
  therefore held the mcp-stdio child's stdin and stdout — which are the
  host's MCP pipes — and its first `[aiui] http listening on …` line
  landed inside the framing stream as a non-JSON frame. Depending on the
  client that is a protocol error, a dropped frame or a dropped
  connection, and it was invisible from our side: aiui's own trace log
  looked perfectly healthy. Holding the write end open also meant the host
  never saw EOF when the child exited, so quitting Claude Desktop could
  hang on the teardown. Every spawn branch now goes through
  `proc_ext::spawn_detached`, which nulls all three streams (and on
  Windows adds `DETACHED_PROCESS`), and the release build no longer
  installs a stdout log target at all — both halves are needed, since an
  inherited handle blocks EOF even with nothing written to it. Present on
  every Windows cold start since v0.10.1; macOS was never affected,
  because LaunchServices gives the new process fresh stdio. Only visible
  behaviour change: running the release binary from a terminal no longer
  streams logs to the console (#181).
- **A failed named-pipe `connect()` was counted as an attached child.** The
  Windows accept loop logged the error and fell through into the attach
  path: it took the unconnected pipe instance as a client, incremented the
  child counter, recorded `ChildAttached` and spawned a reader that failed
  on its first read — producing a phantom detach edge that armed the 5 s
  shutdown grace for a client that never existed. Since
  `is_claude_desktop_running()` reports `false` for any `tasklist` that
  does not succeed, one hiccup inside that window was enough to quit the
  companion with Claude Desktop still on screen. The attach path is now
  unreachable without a connected stream — enforced by control flow, not
  by a runtime check (#181).
- **A persistently failing accept/connect spun at 100 % CPU.** Neither
  loop classified its error or paced its retry, so the non-transient
  failures — descriptor exhaustion on Unix, a pipe name in a bad state on
  Windows — returned instantly and forever: one tokio worker pinned, an
  unbounded append to `/tmp/aiui-trace.log` (which cannot rotate
  mid-process), no new child able to attach while `/health` still answered
  "up", and the 256-entry lifecycle ring overrun in milliseconds, erasing
  the forensic trail it exists to keep. Both loops now reset their failure
  counter on success, retry `Interrupted`/`WouldBlock` immediately so no
  legitimate attach pays for it, and otherwise back off 100 ms doubling to
  a 5 s cap, logging once per backoff step and recording one
  `ChannelAcceptFailing` lifecycle event past five consecutive failures.
  Neither loop can exit the process — the three legitimate exit causes are
  unchanged (#181).
- **`/health`'s WebView probe could not fail.** A user whose dialog window
  was open but frozen — "the dialog opened but nothing happens", the single
  most common stuck report — got `webview.responsive: true` every time, with
  a fabricated `rtt_ms: 0` and no ping ever sent. The probe looked the window
  up by the fixed label `"dialog"`, which the multi-window rewrite had
  already retired: every dialog window's label is its dialog id now, so the
  lookup always missed and the probe always took its "nothing to ping"
  branch. Worse, the frontend had no `ui:ping` listener at all, so the ack
  registry was unreachable in production and repairing only the label would
  have turned a cosmetic lie into a permanent 503 for as long as any dialog
  was on screen. Both halves are fixed together: the probe now resolves the
  newest live dialog via `DialogState::newest_id()` (deterministic, unlike
  picking an arbitrary entry out of the window `HashMap`), `DialogShell`
  answers `ui:ping` with `ui_pong` and unlistens on destroy, the Tauri
  capability scope was widened so per-id dialog windows may actually use the
  ACL-gated event API, and the round-trip budget went from 100 ms — inside
  the window where a freshly mounted WebView is still doing layout — to
  750 ms. With no dialog open, `rtt_ms` is now `null` rather than a `0` that
  reads like a real measurement (#179).
- **A full dialog registry took rendering down for every session sharing the
  companion.** `/health` answered 503 the moment 16 dialogs were pending, and
  the Python bridge preflights `/health` before every render and every
  upload, treating any non-200 as fatal. With a 2 h dialog TTL and parallel
  sessions on one companion, 16 unanswered dialogs is an ordinary state — so
  a session with nothing pending of its own died with
  `RuntimeError("aiui companion /health returned 503: {\"version\":…")`,
  even though `/render` would have recovered on its own by sweeping expired
  entries and evicting the oldest. 503 is now reserved for
  `webview_unresponsive`, the one state that genuinely cannot serve;
  degraded-but-serving states answer 200 with `ready: false` plus a reason.
  The `32`-children threshold is now a named `CHILD_SOFT_CAP` (#179).
- **Both diagnostic paths discarded the diagnosis.** `aiui_health` — the tool
  whose job is to tell a cold companion apart from a rogue process holding
  the port — called `raise_for_status()` and returned
  `{"ok": false, "error": "Server error '503 Service Unavailable'"}`, throwing
  away `pending`, `oldest_age_secs` and `lifecycle_phase`, the whole point of
  a composite response; the Rust bridge collapsed the same body to
  `"/health http 503 Service Unavailable"`. Both now parse and return the
  body on any status (`ok` reports whether the companion answered 200,
  `ready` whether it is healthy), and both `/aiui:health` prompts were
  rewritten to relay the companion's `hint` instead of guessing a cause —
  they used to instruct the agent to blame "WebView frozen" from a field
  that was hardcoded to `true`. A non-JSON body still yields the old
  `{ok: false, error}` shape, so a stranger on `:7777` cannot pass for a
  healthy companion (#179).
- **Uninstall left the aiui entry in Claude Desktop's config on Windows.**
  The remover rebuilt `~/Library/Application Support/Claude/…` inline
  instead of calling the one function that knows the per-OS path, so on
  Windows it looked at a file that does not exist there, reported "nothing
  to do", and left behind an entry whose `command` pointed at a deleted
  binary — Claude Desktop then failed to start a server on every launch.
  Patch and remove now resolve the path through the same function, which is
  the rule a test cannot enforce for you: a test that hands the core its
  path can never catch a wrapper that builds the wrong one (#183).
- **An answered dialog could be accepted and then reported as never having
  existed.** `GET /render/{id}` removed the result slot *before* the HTTP
  response reached the wire, so a tunnel blip while the body was being written
  destroyed the user's answer and the retry got `404 unknown_render_id` — which
  both bridges render as "aiui lost track of render … Restart the dialog". The
  user was told to re-type something they had in fact already submitted,
  including a value typed into a `password` field. Delivery is now idempotent:
  the result is cloned, the slot stays readable for five minutes after the
  first collection, and the bridges retry a transport error on the poll GET
  (never a `404` — that one is terminal) (#193).
- **A dialog nobody was waiting for stayed on the desktop for two hours.**
  Nothing tied a dialog's lifetime to the agent that asked for it: once
  `POST /render` answered `202` the cancellation guard was disarmed and no
  record of "someone is still polling this" existed. Kill the session, drop the
  tunnel or press Ctrl-C and the window sat there until the 2 h TTL — and
  because zombies accumulate, sixteen of them start evicting live dialogs and
  flip `/health` to `503`, which locks out every *other* remote session. The
  result slot is now the dialog's lifetime record, and a background reaper
  cancels a dialog whose caller has not polled for 90 seconds (#193).
- **A clean TTL expiry could be turned into the same misleading 404.** The
  sweep retained slots by `created_at` against the very deadline the resolver
  task uses, and removed them regardless of whether that task had finished, so
  a concurrent render at the boundary could sweep the slot first and the
  documented terminal `{cancelled: true, reason: "ttl_expired"}` silently
  became "the render never existed". The reaper now consults a `done` flag the
  resolver sets, which makes the decision correct rather than merely less
  likely to be wrong (#193).
- **A dialog the user never saw still hung the agent for two hours.** If the
  window failed to build — a label collision behind a not-yet-completed
  `destroy()`, a broken WebView2 runtime on Windows — the error was written to
  the trace log and discarded; `/render` returned `202` as if a window existed
  and both bridges polled in an unbounded loop. Nothing appeared on screen and
  the tool call simply never returned. A deterministic build failure now
  answers `500 {"error": "window_failed", "detail": …}`. A *timeout* waiting on
  a busy main thread is not treated as failure — a false `window_failed` would
  abort a dialog that is about to appear (#193).
- **Pressing Esc in Claude Code did not close the dialog.** The Rust bridge
  dropped every message without an `id` before dispatch, so
  `notifications/cancelled` — the MCP spec's only way to abort an in-flight
  request — was a no-op: the bridge kept polling and kept emitting progress for
  a token the client had already forgotten. Both bridges now retract the dialog
  through the new `DELETE /render/{id}` when their caller is cancelled. The
  route is purely additive, so the wire version stays 1 and an older companion
  simply 404s the call (#193).
- **Quitting the MCP host left the child process alive.** On stdin EOF the
  bridge dropped its writer channel and awaited the writer task — but every
  dispatch task and every progress task held a clone of the sender, and the
  channel only closes when the last one is gone, so the drain waited for as
  long as any tool call was outstanding. With a dialog open that is up to two
  hours, with `lifetime::mcp_attach` still attached and a stale binary in RAM
  answering tool calls. In-flight tasks are now aborted at EOF (their dialogs
  retracted on the way out), progress loops die with their parent, and the
  drain itself is bounded at two seconds (#193).
- **The `upload` file picker opened where nobody could see it, then hung for
  15 minutes.** On macOS the companion runs as an agent app with no Dock icon,
  and macOS will not bring such an app's panels forward — so the picker was
  born behind whatever the user was looking at, with nothing to click and no
  window to Cmd-Tab to, while the bridge held the call for the full 900 s.
  The handler now promotes to Regular mode for the picker's lifetime (via an
  RAII guard, so every exit path demotes), gives the panel a title and a
  parent window when a visible one exists, bounds its own wait at 600 s with
  a `504` that says what happened, and refuses a second concurrent picker with
  a `409` both bridges translate into "another upload is already waiting". A
  dialog closing while a picker is open can no longer demote the app out from
  under it (#194).
- **Uploads to exFAT / FAT32 / SMB destinations failed after the bytes had
  already been transferred.** The never-clobber guarantee came from a hard
  link, which those filesystems do not support — routine on Windows with a
  `target_dir` on a USB stick. The completed transfer was thrown away with a
  message that named no cause. Both bridges keep the hard-link fast path and
  fall back to creating the destination with `O_EXCL`, which preserves
  never-clobber without needing link support (#194).
- **A `~user`-style `target_dir` crashed the Python `upload` tool.** Any
  leading `~` went to `expanduser()`, and `~nosuchuser/x` raises — escaping
  the tool's contract that every failure comes back as `{status, error}`. Only
  `~` and `~/…` expand now, byte-for-byte the Rust rule, and an unavailable
  process cwd returns the documented error dict instead of raising (#194).
- **A local video or audio file was read fully into RAM before anything
  checked its size.** A 3–4 GB screen recording in a `gallery` item was
  materialised before the 512 MB cap was consulted — an allocation failure
  there is an OOM kill of the bridge, which the MCP host reports as the
  thoroughly unhelpful "Server disconnected". Both bridges stat first and skip
  an oversize clip; the Python read moved off the event loop so the progress
  heartbeat keeps firing, and `MemoryError` no longer escapes the best-effort
  handler (#194).
- **Every dialog render left two unhandled promise rejections in the
  window.** The dialog window installed the frontend update-check triggers,
  whose `plugin:event|listen` and `plugin:updater|check` calls were both
  refused by the ACL, with nothing catching either — noise in exactly the
  traces used to debug a hung dialog. The dialog window no longer runs those
  triggers at all: it renders agent-authored content and has no business
  holding the updater, and the headless six-hourly check in Rust already
  detects new releases independently of any window, so update detection is
  unchanged (#195).
- **Uninstall reported that it had removed the local files, and had not.**
  It deleted `token` and `first_run_done`, each behind a `let _ =`, then
  rendered a green "Lokale Dateien entfernt" naming the config directory.
  What survived: `remotes.json`, `gui.lock`, `gui.sock`, and the media cache
  — bounded at 1 GiB of the video the user had aiui show them, and living
  outside the config directory, so the message named neither the files nor
  the place. Worse, the `save_remotes(&[])` call meant to clear the host
  list went through `atomic_write`'s `create_dir_all` and therefore *created*
  `remotes.json`, on Windows inside the stray directory above, during the
  very operation that claimed to have removed it. The sweep now names every
  file explicitly, deletes the media cache too, and reports per path what
  actually happened — a failure reads as a failure and says which path and
  why. The `.bak.<ts>` copies aiui made of the user's *own* config files are
  still deliberately left alone; the hint now says so (#196).
- **Any `gui.lock` failure was reported as "another aiui-GUI holds the
  lock", and the process exited silently.** `try_acquire` fails for two
  unrelated reasons — contention, and any filesystem problem — and the
  caller collapsed both into one trace line plus `exit(0)`: no window, no
  banner, exit code 0. When the cause was not contention (a backup or
  antivirus agent holding the file with a deny-share mode, `LockFileEx`
  failing on a network-redirected roaming `%APPDATA%`, a deny rule on
  `gui.lock`, a leftover `gui.lock` that is a directory) aiui never started,
  the one line a support engineer would read named a second GUI that does
  not exist, and `mcp_attach` respawned the doomed process roughly every
  20 s for the whole session. Contention still exits exactly as before.
  Everything else keeps running *without* the lock and says so in a Settings
  banner — the lock guards the v0.4.43 two-GUIs-in-one-millisecond bind
  race, it is not the last line of defence, and a filesystem error is no
  evidence at all that a second GUI exists. The auto-resurrect spawn now
  backs off (5 s → 30 s → 120 s) instead of firing once per attach cycle
  forever (#196).
- **Dismissing the welcome wizard brought it back two seconds later.**
  `mark_first_run_done` was a bare `let _ = std::fs::write(…)` and
  `dismiss_welcome` returned `Ok(())` regardless, so the frontend hid the
  banner optimistically while `is_first_run` still reported true — and the
  2 s status tick re-opened the whole wizard, on every launch, with the
  error surfaced nowhere. The write is propagated, and the frontend hides
  the section only once it is persisted; a failure is logged once instead
  (#196).

- **A failed update install said nothing at all.** Only the release check
  was wrapped; everything from `downloadAndInstall()` to the relaunch was
  bare. A dead network, a minisign signature mismatch, a full disk and a
  cancelled admin prompt therefore all ended the same way: the spinner
  stopped, no message appeared, the banner still offered the update, and
  the app never changed. That is the one support signature a maintainer
  cannot work with — "aiui never updates", with nothing in the UI log to
  say why. Every failure now raises a native error modal naming the cause
  and lands in the Settings log; `checkForUpdates` returns its outcome
  instead of swallowing it (#197).
- **Clicking Install while a dialog was open destroyed that dialog.** The
  guard written to prevent exactly this, `is_update_safe_to_install`, had
  had no caller since v0.4.44 — the install path went straight from the
  confirmation to `downloadAndInstall()` and a relaunch. A user installing
  from the banner while a remote agent had a form open tore that window
  down mid-`/render`: their typed content was gone and the agent got a
  cancelled result. Both install paths — the Settings button and the
  agent-facing `/update` — now check the dialog registry first and defer,
  leaving the banner in place (#197).
- **Double-clicking "Check for updates" started two installs.** The footer
  button called `checkForUpdates` as a floating promise and never set
  `busy`, so its own `disabled={busy}` was decoration and two concurrent
  `downloadAndInstall()` calls could race over the same bundle — on macOS
  that is two renames over the live `.app`. It now routes through the same
  guarded entry point as the banner, backed by a module-level latch (#197).
- **Windows updates leaked an SSH tunnel and broke `/aiui:update`.**
  `tauri-plugin-updater` ends its Windows install with
  `std::process::exit(0)`, which bypasses `ExitRequested` — so the exit
  cleanup never ran and every update left the previous instance's
  `ssh -NTR` child alive. The relaunched instance then saw the port already
  forwarded and pinned itself to "connected (shared forward)" for the rest
  of its life, with its own forward never binding; only a manual `taskkill`
  or a reboot recovered it. The startup sweep could not reclaim the child
  either, because it identified orphans by `ppid == 1`, which is a POSIX
  re-parenting rule that does not hold on Windows. Cleanup now runs from
  the plugin's `on_before_exit` hook, and the sweep uses "parent is gone
  from the process table" off POSIX. Same root cause, second symptom: the
  `/update` endpoint built its response *after* the install, i.e. after the
  process had already exited, so the Python bridge raised a transport error
  instead of returning the version delta — `/aiui:update` was structurally
  broken on Windows. It now answers first and installs after (#197).
- **The pending-update banner could not be got rid of.** Its type comment
  promised it clears "once the user installs or the on-disk version catches
  up"; the second half was implemented nowhere. After a release was yanked
  — or a `latest.json` stopped advertising the platform — the banner
  offered a version the updater would no longer hand out, clicking Install
  answered "you are on the current version", and the banner came straight
  back. It is deliberately dismiss-free, so the only way out was restarting
  the companion. All four paths now retract it: a successful install, a
  manual check that finds nothing, the headless six-hour check, and the
  status poll once the installed version has caught up (#197).
- **The post-render update trigger had never fired in production.** The
  `update:check` event was emitted only on the synchronous render path,
  past the async branch's `return` — and both shipping bridges request the
  async path unconditionally, so the emit was unreachable for every real
  caller. Updates were not stalled (the per-window mount check and the Rust
  six-hour loop still ran), but the trigger that clusters checks around
  actual use was gone, and nothing would have revealed it. The emit moved
  into `resolve_dialog`, the single point both paths run through (#197).
- **The whole update surface was hardcoded German.** aiui auto-detects its
  locale and ships a complete English catalogue, yet every string in the
  update flow was a German literal — and these are *native OS modals*, the
  most prominent text the product shows. An English-locale user got German
  dialogs with no explanation. Both HTML entry points also declared
  `lang="de"` unconditionally, which gave English sessions German
  screen-reader pronunciation and German spellcheck inside every `form`
  textarea. The update strings, the add-remote dismiss button and the
  quit-failure message are now translated, and the document language
  follows the resolved locale (#197).
- **A green status dot over a Claude config that could never work.** The
  `is_*_current` health predicates stopped at the entry's `command` and
  never looked at `args`, while the patchers write `command` **and**
  `args`. An `aiui` entry missing `--mcp-stdio` therefore showed a green
  dot and passed the welcome-wizard check — but that binary starts the
  Tauri GUI instead of speaking MCP on stdio, so Claude waited forever on a
  process that never answers the protocol. And because the same predicate
  is the repair gate in the launch-time setup path, the broken entry was
  never healed: there was no way out from inside the app. Predicate and
  patcher now share one definition of "current", and a **Repair config**
  button sits next to the red dot (#198).
- **aiui registered a path that stops existing.** Launched straight off the
  mounted DMG or out of `~/Downloads`, Gatekeeper App Translocation hands
  the app a `/private/var/folders/…/AppTranslocation/<uuid>/…` path that is
  gone when it quits and carries a fresh UUID next time. That string went
  verbatim into all three host configs, so every tool call failed with "no
  such file" — and since the random path never matched, all three files
  were rewritten and three fresh `.bak.<ts>` copies dropped on *every*
  launch, forever. Nothing in the UI or the trace log named the cause.
  aiui now detects the ephemeral location, refuses to register, and shows a
  banner asking the user to move the app to Applications and relaunch.
  Substituting the canonical `/Applications` path would only swap a broken
  path for one pointing at nothing (#198).
- **A Windows uninstall left Claude Desktop permanently broken.** The
  removal counterpart rebuilt the macOS config path by hand
  (`~/Library/Application Support/Claude/…`) while its own patcher went
  through the per-OS helper. On Windows that path never exists, so
  Uninstall printed the green line "claude_desktop_config.json existiert
  nicht, nichts zu tun." while `%APPDATA%\Claude\claude_desktop_config.json`
  kept pointing `mcpServers.aiui` at an `aiui.exe` the user was about to
  delete — leaving Claude Desktop with a permanent MCP-server-failed error
  and no aiui left to fix it. The legacy `aiui-local` key was never cleaned
  up there either. Both halves now take the path from the same helper, so
  the drift cannot return (#198).
- **`~/.ssh/config` was backed up and rewritten on every launch.** The
  legacy-forward cleanup computed whether anything changed but used it only
  to word the message — the backup and the write ran unconditionally, once
  per registered remote, on every GUI start. A user with two remotes
  collected two more full copies of their ssh config per launch. Worse, the
  file was re-serialised through `str::lines()`, which drops the `\r` of a
  CRLF pair: a `~/.ssh/config` written by a Windows editor was silently
  converted to LF-only, and a file without a trailing newline grew one.
  Nothing is touched now unless there is really an aiui line to remove, and
  surviving lines come back byte-identical (#198).
- **Skill removal always reported success, and a failed `ssh` reported the
  wrong error.** Uninstall discarded both filesystem results and returned
  green regardless, so a read-only home showed a clean uninstall while
  `~/.claude/skills/aiui/SKILL.md` stayed on disk and Claude Code kept
  loading the skill for a product the user had just removed. And in the
  remote skill install, the `mkdir` result was inspected only on the branch
  where `ssh` actually started — when it could not be spawned at all (no
  OpenSSH client on Windows, say), execution fell through and the user was
  shown the subsequent `scp` error instead of the real cause (#198).
- **Claude Desktop's config was rewritten on every launch even when
  correct.** It was the last patcher without the already-current
  short-circuit its two siblings gained in #182, so it dropped a fresh
  timestamped backup on each start for no change at all (#198).
- **A pull request based on another branch got no CI checks at all.** The
  workflow's `pull_request.branches: [main]` filter matches the *base*, so
  a stacked PR — the normal shape of a multi-step change — produced no
  build, no tests, no clippy, no drift guard. Not a failure, not a skip:
  nothing, which at a glance looks the same as passing. That removed the
  safety net from exactly the change shape most likely to need it, since a
  stacked PR is by construction the one touching code another unmerged
  change also touches. Two of the nine PRs in this release were in that
  state (#222).
- **README and SECURITY.md promised automatic updates that never happened.**
  Both said the in-app updater installs patches on its own. Nothing in the
  product has done that since v0.4.44 — the headless check and the silent
  frontend check both *record* an available version and stop. The only
  thing that installs is a click on the settings banner or `/aiui:update`.
  And aiui runs headless by design: no dock icon, no menu-bar item, nothing
  inviting the user into Settings. So a user who never opened that window
  never learned an update existed and stayed on the installed version
  indefinitely, security fixes included, while `SECURITY.md` told security
  reporters those fixes were delivered automatically. Both documents now
  describe what the code does (#188).
- **Mermaid flowchart node labels rendered empty.** Mermaid 11 puts
  flowchart labels in HTML inside `<foreignObject>`, and the sanitiser runs
  with `USE_PROFILES: { svg, svgFilters }` — a DOMPurify *profile* replaces
  the allow-list rather than adding to it, and `foreignobject` is both
  outside the svg profile and in `DEFAULT_FORBID_CONTENTS`, so the element
  and its text were stripped. Diagrams came out as boxes with nothing in
  them. The v0.4.38 change that dropped `foreignObject` from `FORBID_TAGS`
  was therefore a no-op — nothing it forbade was reachable anyway — and
  the regression it was written to fix stayed broken.
  Fixed at the source: `htmlLabels: false` keeps labels in `<text>` /
  `<tspan>`, which the svg profile does allow. Deliberately **not** fixed
  by widening the sanitiser: admitting `<foreignObject>` means admitting
  `<div>`, `<span>`, `<img>` and `<a href>` inside the SVG, for content an
  agent supplied. Cost: long labels use Mermaid's own line-breaking rather
  than HTML word-wrapping (`<br/>` still works), and FontAwesome `fa:fa-x`
  substitution in labels stops resolving. Neither was documented (#189).
- **A link in agent-supplied markdown destroyed the dialog.** A plain
  `[text](https://…)` in a `markdown` field or a `compare` variant
  survived sanitisation, as it should — but clicking it navigated the
  **dialog window itself** away from `dialog.html`. The Svelte app was
  gone, the dialog's `/render` hung until its two-hour TTL, and the user
  was left with a web page in a frameless window and no way back. A
  compromised remote host could aim that anywhere. The window now refuses
  to navigate anywhere but its own app origin, and the frontend hands
  `http(s)` links to the user's default browser instead — which is what
  `skill.md` had been claiming all along (#189).
- **Mermaid diagrams rendered twice on mount.** `onMount` and an
  `initialised`-gated `$effect` both drove the first render, and the
  effect's guard read a flag the render itself sets. The effect now owns
  both the first render and later source changes (#189).
- **A hung probe could park a tunnel task forever.** `ConnectTimeout=5`
  bounds the TCP connect only; authentication, a wedged remote shell or a
  stalled curl were unbounded after that, and the probe is awaited inside
  the poll loop. It now has a 15 s overall cap, is killed on drop rather
  than orphaned, and races against cancellation at both call sites — a
  remote removed while a probe was in flight used to wait for it (#187).
- **The probe called outcomes that prove nothing.** A missing token on the
  remote, a curl that timed out, a remote without curl, an ssh killed by a
  signal: all four were reported as "the forward is gone", which in
  shared-forward mode means a retry storm against a port that is still
  occupied. Each is now inconclusive, which costs one extra poll. The
  missing-token case gets its own exit code so it cannot be confused with
  a real answer — matching what the function's own docstring always
  claimed (#187).
- **Every tunnel failure surfaced as "ssh exit code 255".** ssh explains
  itself on stderr, and stderr went to `/dev/null`. It is now captured
  into a bounded ring and the last lines are appended to the status, so
  Settings shows e.g. `ssh exit code 255 — remote port forwarding failed
  for listen port 7777` instead of the least informative thing ssh can
  say (#187).
- **The reconnect backoff never reset.** It only doubled, so a link that
  connected, worked for hours and then dropped inherited whatever the last
  startup stumble had left behind — a flapping connection degraded into a
  permanent 30 s hole during which every remote dialog fails. A link that
  survived past 30 s now starts over at 1 s, and the sleep carries ±20 %
  jitter so several tunnels that drop together do not retry in lockstep
  (#187).
- **The companion quit after every dialog for anyone whose host is not
  Claude Desktop in `/Applications`.** The exit authority was
  `explicit || !is_claude_desktop_running()`, and liveness was probed with
  `pgrep -f /Applications/Claude.app/`. For a Claude-Code-only or
  Codex-only user — both first-class hosts aiui registers itself with — or
  for a Claude Desktop installed in `~/Applications`, that predicate was
  permanently true, so the default-**deny** exit gate inverted into
  default-**allow**. The first dialog submit closed the only window, Tauri
  fired `ExitRequested`, and the host exited: the v0.4.42 "lost the GUI
  ~18 ms after submit" regression, re-opened for a whole class of users.
  On a Mac serving only remote sessions there was no local child to
  relaunch it, so aiui stayed dead until someone touched the machine.
  The Wirt signal is now host-agnostic — "Claude Desktop is not my Wirt"
  means *stay* — the macOS probe matches the bundle executable at any
  install location (and still never the `claude` CLI), and a
  last-window-close (`code: None`) can no longer terminate the host at all
  (#180).
- **Multi-instance exits left a zombie GUI and killed the live instance's
  tunnels.** The four "another aiui already serves this" paths called
  `app.exit(1)`, which the default-deny gate vetoed — leaving a process
  holding a pipe it could not serve — and then swept *all* ssh-NTR
  tunnels, although a losing instance never opened one: they belonged to
  the instance that won. Exits now route through one `terminal_exit` that
  uses `std::process::exit` and a sweep scoped by reason (#180).
- **In-flight dialogs are resolved before the host exits.** Every terminal
  path now drains the registry, sending each pending `/render` a
  `{cancelled: true, reason: "host_exiting"}` and destroying its window,
  then gives Axum a moment to flush — instead of leaving callers to hang
  until their own timeout (I5/I7). The grace decision also counts open
  dialogs: one on screen is proof someone still needs the host (#180).
- **A transient named-pipe rotation failure no longer kills a healthy
  Windows host.** It is retried five times with doubling backoff, and only
  a persistent failure exits — cleanly, through the drain (#180).
- **Both bridges forward *why* a dialog was cancelled.** The companion
  distinguishes `host_exiting`, `ttl_expired`, `evicted` and
  `channel_dropped`, but each bridge flattened them into a bare
  `{"cancelled": true}` — indistinguishable from the user pressing Escape,
  so an agent retried the wrong thing (#180).
- **A working remote broke on the next launch, and aiui reported success.**
  `add_remote` probes the remote for an **absolute** `uvx` path and pins it
  — the bare name depends on Claude Code's PATH at spawn time, which is
  exactly what fails on the hosts the probe's fallback list exists for. That
  discovery was then thrown away. The startup resync (every remote, every
  launch) and the Settings "Resync" button both passed no path, so the
  script rewrote the pinned absolute path back down to `"uvx"` and logged a
  green result. The next Claude Code session on that host could not find
  `uvx`, and every aiui tool call failed with "command not found".

  A one-way downgrade, not a flip-flop: once the bare name was written,
  later resyncs saw it as current and left it. The absolute-path discovery
  had effectively been dead code since 0.4.29, when the auto-resync was
  added.

  The discovered path is now remembered per host and passed by both resync
  paths. Two further guards: "no path known" no longer means "write the
  bare name" but "keep an existing resolvable command and re-pin only the
  version", which protects hosts upgrading from ≤ 0.10.1; and the Resync
  button self-heals a host with no remembered path by re-probing, which
  also gives that button the pre-flight check it never had (#184).
- **An unparsable host config was replaced, not repaired.** Every local
  writer of `~/.claude.json` and `claude_desktop_config.json` treated a
  parse error as "empty file" and then wrote a document containing only
  aiui — taking every other MCP server, every project entry and the OAuth
  block with it. One trailing comma was enough, and the same hole sat in
  both remote scripts, where the remove path additionally had no backup at
  all. All six writers now stop before touching a file they could not
  parse, and say so. An empty file still parses as `{}`, which is
  genuinely safe. The remote scripts share one preamble (parse-or-bail,
  backup, atomic tmp+replace) so they cannot drift apart again, and both
  writes are atomic — they used to open-truncate-write a file a live
  Claude Code session may also be writing (#182).
- **The aiui entry no longer eats its own neighbours.** Registration built
  a fresh `{command, args}` object and overwrote whatever was there, so an
  `env` block or any other key on the entry vanished on the next launch.
  Only those two keys are written now, in all three host configs — the
  Codex TOML path keeps the user's other keys *and* their comments. The
  idempotency check compares only those two keys as well, so an entry
  carrying extras is no longer rewritten, and re-backed-up, on every single
  launch (#182).
- **Backups are findable and bounded.** `~/.claude.json` backed up to
  `~/.claude.bak.<ts>` — a name that looks like a backup of a different
  file — at second granularity, so two writes in the same second silently
  overwrote one another, and the pile grew without limit. Backups are now
  `<file>.bak.<ms>`, capped at five per target, and the path appears in
  the step result so the user can actually find it (#182).
- **A failed registration at startup is traced instead of discarded.**
  Three call sites dropped their result with `let _ =`; with the parse-error
  stop in place that would have turned a silent wipe into a silent no-op
  (#182).
- **`diagnose-session-startup.sh toggle-aiui` no longer restores a stale
  whole-file copy.** It stashed a copy of `~/.claude.json`, then `mv`'d it
  back — throwing away everything Claude Code wrote during the measurement
  window the script itself asks the user to create. It now stashes only the
  removed entry, re-reads the current file on restore, writes atomically,
  and warns when a per-project aiui entry would keep aiui loaded for the
  project being measured (#182).
- **Windows: the diagnostic trace was silently discarded.** `TRACE_PATH`
  was the hard-coded `/tmp/aiui-trace.log`, which does not exist on
  Windows, so every `logging::trace()` line was dropped and the failed open
  was swallowed — the platform with the youngest port shipped with no
  diagnostics. The trace now resolves per-OS (`%LOCALAPPDATA%\aiui\logs`
  on Windows, `<config dir>/logs` elsewhere), is opened `0600` with
  `O_NOFOLLOW` so a planted symlink cannot redirect it, reports a failing
  open once on stderr instead of never, and names its resolved path in
  every session header. `/tmp` was also the wrong place on Unix: it is
  world-readable and shared (#185).
- **A form's Cancel button could blank the user's credential file.**
  `target` file writes fired on *every* action except the built-in
  `__cancel__`, so a custom `Cancel` or `Save draft` action — the pattern
  `skill.md` itself recommends — committed the write with whatever was in
  the field. For an untouched `secret` that is the empty string, so
  `mode: "create"` with `overwrite: true` replaced the file with zero
  bytes, and `mode: "substitute"` erased its own placeholder so even a
  retry failed. The agent was told `{written: true, bytes: 0}`. Two guards
  now close it, in both writers (native app and Python bridge): an action
  carrying `skip_validation: true` no longer commits target writes, and an
  empty value is refused before the filesystem is touched. A field missing
  from the payload is reported as such instead of being laundered into a
  blank write (#177).
- **A release published a macOS-only update feed, breaking every Windows
  client's update check.** `releases/latest/download/latest.json` is what
  every installed aiui polls, and `release-macos.yml` published one
  carrying only `darwin-aarch64` — the `windows-x86_64` entry was added
  later by a second, manually dispatched workflow. In between, a Windows
  update check did not report "up to date"; it failed
  (`tauri-plugin-updater` raises `TargetsNotFound` for a missing target),
  and if the second dispatch was forgotten it failed permanently. That is
  what shipped in v0.10.1.

  The release is now created as a **draft**, which
  `releases/latest/download/…` does not serve, so clients keep resolving
  the previous complete feed until both platforms are in. The Windows
  workflow is dispatched automatically, validates the assembled feed, and
  only then publishes the release. A Windows failure therefore holds back
  the whole release rather than shipping half of one — the correct
  coupling for a two-platform product.

  Deliberately **not** fixed by carrying the previous release's Windows
  entry forward: that feed would advertise the new version while pointing
  at the old installer, which verifies and installs cleanly and leaves the
  client on the old version — a silent reinstall loop, with `/update`
  reporting success for a version the machine never reached (#190).
- **`release-windows.yml` attached no artifacts.** Its first ever run —
  the v0.10.1 release — failed at the artifact lookup. Tauri signs the
  NSIS installer in place (`…-setup.exe` plus `…-setup.exe.sig`); the
  step looked for a `.nsis.zip` intermediate that no longer exists and
  reported it as `check TAURI_SIGNING_PRIVATE_KEY` — misleading, since
  the key had worked and that is why the `.sig` existed. The lookup now
  matches the installer and derives the `latest.json` url from the
  produced filename, so the url can't desync from the signed artifact
  (#175). The v0.10.1 Windows artifacts were shipped by a re-dispatch
  after this fix.
- **The Windows installer was bundled by a Tauri CLI nobody had pinned.**
  Both Windows jobs ran `npm ci` and then threw away its result for a
  global `npm install --global @tauri-apps/cli@^2`, resolved at build time
  and discarded — not the `2.10.1` that `companion/package-lock.json`
  locks. So two dispatches of the same tag could bundle with two different
  bundlers, the `.sig` the updater verifies was produced by whichever one
  npm happened to pick, and "which bundler built the `.exe` user X is
  running?" had no answer at all. Both jobs now build with `npx tauri`,
  which is what the macOS job always did, and a step asserts the CLI
  version against the lockfile before the build (#192).
- **The README promised reproducible builds the build could not deliver.**
  "builds reproducibly" sat in the FAQ beside the true statements about
  Developer-ID signing and notarization, which lent it credibility it had
  not earned: `build.rs` stamps wall-clock `chrono::Utc::now()` into every
  binary, so two builds of one commit are byte-different by construction.
  That is the worst kind of security documentation — it invites a sceptical
  user to run a check that cannot succeed. The sentence now says what
  actually holds (built only in public GitHub Actions, from SHA-pinned
  actions and a pinned compiler), and a new unit test locks the
  `build_info` format that `/version` and every trace log expose (#192).

### Security

- **Every third-party action in every workflow is pinned to a commit
  SHA.** The macOS release job holds the Developer ID certificate, the
  App Store Connect notary key, the minisign updater private key and the
  PyPI publish token in one environment, with `contents: write` on the
  repo — and it ran `actions/checkout@v4`, `Swatinem/rust-cache@v2`,
  `astral-sh/setup-uv@v3` and `dtolnay/rust-toolchain@stable`, the last of
  which is a *branch* ref and so expected to move. One taken-over action
  repo or one force-moved tag was enough to read those secrets out of the
  job; with the minisign key an attacker can sign an `aiui.app.tar.gz`
  that every installed companion accepts from the updater feed, and aiui
  installs its own updates, so there is no user-visible install step to
  catch it. The blast radius was every installed companion, not just the
  next download. Also pinned: the Rust compiler and the uv binary, which
  used to be downloaded as `version: latest` into the same job (#192).
- **`POST /media` could mint any Content-Type on the API's own origin.** The
  `ext` parameter went through a sanitiser that kept any five lowercase
  alphanumerics, and `ServeDir` derives the served `Content-Type` from it — so
  `?ext=html` produced a `text/html` document on `127.0.0.1:<port>`, the same
  origin as the authenticated API, reachable through an unauthenticated
  capability URL. A prompt-injected agent holding the token could put that URL
  in front of the user. The extension is now an allowlist of the ten types the
  widgets actually play, everything else stores as `bin`, and `/media/blob`
  responses carry `X-Content-Type-Options: nosniff` and
  `Content-Security-Policy: sandbox` (#194).
- **A dialog window could invoke every one of aiui's commands.** Tauri only
  ACL-checks `plugin:`-prefixed commands unless the app ships its own ACL
  manifest, and aiui ships none — so `capabilities/` said nothing about
  `uninstall_all`, `quit_app`, `authorize_exit_for_update`, `add_remote`,
  `remove_remote` or `status`, and the one window that renders content an
  agent on a remote host wrote could call all of them. Not a live exploit
  (agent content passes through DOMPurify first), but the second barrier was
  missing, and the blast radius behind it was the whole install plus the
  user's SSH aliases. Those commands are now Settings-only, gated in Rust by
  `is_privileged_window`. `open_url` stays reachable on purpose: links in
  agent markdown have routed through it since #189, and it opens an http(s)
  URL in the browser and nothing else (#195).
- **One session's dialog could read or answer another's.** `get_dialog_spec`,
  `dialog_submit`, `dialog_cancel` and `write_dialog_targets` took the dialog
  `id` on trust and never compared it with the calling window's label — which
  *is* its dialog id. With two dialogs open, one window could pull the other's
  spec, including a `form` holding a secret. Exactly the isolation the
  one-window-per-render model was introduced to provide; it is now enforced
  (#195).
- **`capabilities/default.json` was scoped to a window that no longer
  exists.** The multi-window refactor gave every dialog window its own UUID
  label, and the file still listed the literal `"dialog"` — a glob that has
  matched nothing since. It read as if dialog windows were permissioned while
  granting them nothing, and the Settings-side plugins (`updater`, `process`,
  `dialog`, `notification`) were nominally offered to them. `default.json` is
  now `"windows": ["setup"]`; the new `capabilities/dialog.json` covers dialog
  windows with `core:event:allow-listen`/`allow-unlisten` and nothing else —
  no emit, so agent content cannot forge an event into the Settings window
  (#195).
- **`<style>` is now forbidden in rendered Mermaid SVG.** Mermaid's
  `classDef` directive turns caller-supplied text into emitted CSS, and
  the svg profile does not exclude `<style>`. Attacker-controlled CSS in a
  dialog window is UI redressing — these are the windows where the user
  clicks Confirm on destructive actions (#189).
- **The API token was readable from the remote host's process list.** The
  shared-forward probe interpolated the token into
  `curl -H "Authorization: Bearer $T"`, so the remote shell expanded it
  before exec and the live token sat in curl's `argv` — visible in `ps` to
  every user on that machine, once per poll (every 30 s in shared-forward
  mode). Anyone who read it could render dialogs on the user's desktop
  through the tunnel. The header now reaches curl over stdin, so the token
  never becomes an argument of any process. Deliberately not via a pipe
  from `printf` (external where it is not a builtin, which just moves the
  leak) nor through the environment (`/proc/<pid>/environ` is readable by
  the same users as `cmdline`) (#187).
- **Every user file aiui rewrites came back with the wrong permissions.**
  `atomic_write` created a fresh temp file (umask-masked `0666`) and renamed
  it over the destination, discarding the destination's own mode. Claude
  Code creates `~/.claude.json` as `0600` because it holds OAuth data and
  other MCP servers' `env` blocks; after one aiui config patch it was
  world-readable, permanently. `~/.ssh/config` got the same treatment on
  every launch, and under a `002` umask became group-writable — which makes
  OpenSSH refuse to run at all. The destination's mode is now copied onto
  the temp handle *before* the rename, so there is no world-readable window
  either. A symlinked config (a dotfiles-repo setup) is no longer replaced
  by a regular file: the link is resolved first and written through, and a
  symlink cycle fails loudly instead of silently swapping the link for a
  file (#185).
- **The API token is validated and kept at `0600`.** A malformed token
  (empty, truncated, not 64 hex chars) is now regenerated instead of being
  used, `0600` is re-asserted on every launch so a token restored from a
  backup gets tightened, and the config directory itself is created `0700`.
  The GUI lock file is created `0600` and repaired if an older install left
  it `0644`. The Python bridge refuses a malformed token with a clear
  message instead of sending a bare `Bearer ` (#185).
- **Token comparison no longer leaks its prefix through timing.** `auth_ok`
  folds every byte instead of returning at the first mismatch, and an empty
  configured token now authenticates nobody rather than accepting an empty
  `Bearer ` (#185).
- **Windows: another local process could take `127.0.0.1:7777` from us.**
  `SO_REUSEADDR` means the opposite thing on Windows — it lets an unrelated
  process bind a socket we are already listening on and answer the bridges
  in our place. Windows now gets `SO_EXCLUSIVEADDRUSE` instead, which
  reserves the address; Unix keeps `SO_REUSEADDR` for the TIME_WAIT reason
  it was added for (#185).
- **Render specs are no longer written verbatim to the trace file.** A spec
  carries the user's own content — question text, pre-filled defaults, file
  paths, `target` destinations — and the trace is what people paste into
  public bug reports. The trace now records the shape (kind, field and
  option counts, byte size); `AIUI_TRACE_SPECS=1` restores the full body
  when it is genuinely needed (#185).
- **A `secret` field without a `target` handed its plaintext to the agent.**
  Both `skill.md` and the `form` tool description promise a `secret` value
  is "NEVER returned to you", but the stripping keyed on the presence of
  `target`, not on the field kind — and `target` is a nested object that is
  easy to omit. A `secret` without one sailed through validation (there was
  even a unit test asserting that shape valid), looked identical to a
  `password` in the UI, and its value went back in `result.values` and thus
  into the transcript. Three layers now close it: the companion rejects the
  shape with `invalid_spec`, and both the frontend and the Python bridge
  strip on the kind rather than on the target, so an older companion in
  front of a newer bridge cannot leak either. A target-less `secret` is
  rejected rather than silently downgraded to `password`: handing over a
  credential the docs promised would never be returned has to be loud
  (#186).

- **The Python bridge discarded the reason for a rejected spec.**
  `raise_for_status()` threw away the companion's `{error, detail, hint}`
  body, so a remote agent got a bare `HTTPStatusError: 422` while a local
  Rust-bridge agent got the explanation. The reason now survives the bridge
  (#186).

## [0.10.1] — 2026-08-31

### Added

- **First Windows release.** aiui ships a Windows installer for the first
  time: `aiui_0.10.1_x64-setup.exe`, built and signed for the updater on a
  `windows-latest` runner by `release-windows.yml`, attached to this
  release alongside the macOS artifacts. `latest.json` now carries a
  `windows-x86_64` entry, so the in-app updater serves Windows too — until
  now it had only `darwin-aarch64`, and an update check on Windows landed
  in the error path rather than reporting "up to date".

  The Windows port itself has been buildable since v0.6.0 and was exercised
  by beta testers on per-push CI installers; what was missing was the
  release path, not the app. The installer is **not** Authenticode-signed,
  so SmartScreen warns on first launch — "More info" → "Run anyway". That
  is a deliberate v1 decision; an EV certificate is a separate concern.

### Fixed

- **The release workflow is re-runnable.** A run that pushed the version
  tag and created the GitHub release but then failed at the PyPI step —
  e.g. a rejected `UV_PUBLISH_TOKEN` — could not be recovered by
  re-dispatching, because the re-run aborted at the tagging command. Both
  halves now probe for existing state first, so a recovery dispatch
  proceeds to the step it was re-dispatched for (#172).

### Changed

- **Release and Windows documentation corrected** (#173). CONTRIBUTING.md
  still documented a local sign-and-notarize pipeline with its environment
  variables as the way to ship, years after `scripts/release.sh` became a
  stub that refuses to run; `release-windows.yml` told the reader the
  GitHub release is created by that script "from the maintainer's
  machine". Both now describe the two `workflow_dispatch` pipelines that
  actually do the work. The README FAQ no longer claims interactive
  Windows testing is outstanding.

## [0.10.0] — 2026-08-17

### Added

- **OpenAI Codex / ChatGPT is now a first-class host (#168).** aiui
  registers itself with Codex the same self-contained way it already does
  with Claude Desktop and Claude Code — on launch it writes its own
  `~/.codex/config.toml` entry (`[mcp_servers.aiui]`, pointing at the
  bundled `--mcp-stdio` server, no `uv`/`uvx`). No manual config, no
  Claude-specific glue: install aiui.app and Codex sees the
  `confirm`/`ask`/`form` dialogs immediately. The `~/.codex/config.toml`
  edit is format-preserving (`toml_edit`), so any comments and other MCP
  servers the user has are left untouched.

### Changed

- **Host registration is now gated on detection (#168).** aiui writes a
  host's MCP config only when that host is actually installed — Claude
  Desktop (app bundle or config dir), Claude Code (`~/.claude.json` /
  `~/.claude/` / `claude` on `PATH`), or Codex (`~/.codex/` / `codex` on
  `PATH`) — instead of writing the Claude configs unconditionally. No more
  phantom config files for hosts you don't use. It re-checks on every
  launch, so a host you install later is picked up automatically on the
  next start.

### Fixed

- aiui no longer rewrites `~/.codex/config.toml` (and drops a fresh
  timestamped `.bak`) on every launch when the entry is already correct —
  the same idempotent short-circuit the Claude paths already had (#170).

## [0.9.0] — 2026-08-02

### Added

- **File upload from the Mac into the agent session (`upload` tool +
  `/aiui:upload` slash-command, #146).** The first aiui data flow that
  runs *Mac → agent-host* instead of the other way around. Calling
  `upload` opens a **native file picker** on the user's Mac (via the
  Tauri dialog plugin); the chosen file streams back over the existing
  authenticated `:7777` channel — the reverse direction of `POST /media`
  — and is written to `target_dir/<filename>` on the host the agent runs
  on (the remote for an SSH session). It replaces the "please `scp` me
  that file" round-trip. `target_dir` is optional (defaults to the
  bridge's cwd); the filename comes from the selection, so the write is
  deterministic with no temp/staging path. Existing files are never
  overwritten — a name clash returns an error rather than clobbering.
  Robust error paths (`{status:"error", error}`) cover a cancelled
  picker, an unreadable file, the 512 MB size cap (413), and a
  missing/unwritable target directory. The tool blocks until the user
  picks or dismisses the picker, with the usual `notifications/progress`
  keepalive. Implemented consistently in the native Rust MCP server
  (`companion/src-tauri/src/{mcp,http}.rs`) and the Python bridge
  (`aiui-mcp`, used by remote SSH hosts via `uvx`), plus a new
  `/aiui:upload` prompt and an `INSTRUCTIONS` trigger on both.
- **`compare` tool — side-by-side A/B (or A/B/C) content compare (#23).**
  New standalone MCP tool (alongside `confirm`/`ask`/`form`/`gallery`) that
  renders 2+ variants as equal-width panes next to each other and lets the
  user click one to pick, instead of a small thumbnail (`ask`) or a
  per-item batch review (`gallery`). Each variant carries a stable `value`
  plus `content` (Markdown text — drafts, diffs, code) and/or `src`
  (image/video, the same resolution rules as every other aiui image
  field). Optional `sync_scroll` locks scroll position across all panes
  for long-text compares; `max_height` set on any variant caps every
  pane's height so they stay visually equal. Returns `{cancelled,
  selected}`. New `Compare.svelte` widget, `compare` case in the Rust
  spec validator + window-size estimator, and a mirrored `compare` tool
  in the Python `aiui-mcp` package for remote/headless sessions. Extracted
  the Markdown-to-sanitized-HTML renderer (previously private to
  `Form.svelte`) into `companion/src/lib/markdown.ts` so both widgets
  share one DOMPurify allowlist instead of drifting.
- **New form field `annotated_image` — mark a point or region on an
  image (#24).** When the answer the agent needs is spatial ("where should
  the logo go?", "which part do I crop?", "point at the bug in this
  screenshot"), words are a poor carrier. The field shows an image and lets
  the user mark it directly: `mode: "point"` (click a crosshair marker,
  default), `mode: "region"` (drag a rectangle), or `mode: "both"` (a
  Point/Region toggle; both can be set). The `src` reuses the existing image
  resolution (absolute / `~/` local path inlined by the bridge, `http(s)://`
  fetched Mac-side, or `data:`). The result — under the field `name` —
  carries **normalized 0..1** coordinates: `{point: {x, y} | null, region:
  {x, y, w, h} | null, natural: {width, height} | null}`, resolution
  independent, with `natural` (the image's intrinsic pixel size) so the agent
  can recover pixel coordinates losslessly. `required` gates the submit
  action on an annotation being present. Documented in the skill field
  catalog and mirrored in the Rust and Python `form` tool descriptions.
- **`audio` field in `form` dialogs (#25).** Native `<audio controls>`
  player for TTS-sample review, voice-memo confirmation, or triaging a
  generated sound clip — read-only like `image`/`mermaid`/`wireframe`,
  no value in the form result. Spec: `{kind: "audio", src, label?}`.
  `src` accepts the same three formats as `image` (`data:`, `http(s)://`,
  absolute/`~/` local path), but a local audio file
  (mp3/m4a/wav/aac/ogg/flac) is never inlined as `data:` — it's always
  routed through the same size-unbounded `/media` cache the `gallery`
  tool already uses for local video, so clips of any size work
  identically whether the agent runs on the user's Mac or a remote SSH
  host. Both bridges (Rust companion and the `aiui-mcp` PyPI package for
  remote/headless use) implement the same routing so behavior doesn't
  drift between the two.
- **`notify` tool — native macOS notification (#17).** A fire-and-forget
  async-completion signal for the agent ("tests green", "deploy done",
  "merge conflicts, need you") that, unlike `confirm`/`ask`/`form`/
  `gallery`, does not block on a user response: the companion hands the
  notification to macOS's `UNUserNotificationCenter` (via
  `tauri-plugin-notification`) and the tool call returns `{ok: true}`
  immediately — no dialog window, no registry entry, no progress
  keepalive. Takes `title`/`body` (required) plus optional `subtitle` and
  `sound`. New `POST /notify` companion endpoint, registered in both the
  native Rust MCP server (`mcp.rs`) and the Python remote bridge
  (`aiui_mcp/server.py`) with matching behavior. First send triggers the
  one-time macOS notification-permission prompt, same as any native app.

### Changed

- **`docs/skill.md`** documents `notify` alongside `confirm`/`ask`/`form`/
  `gallery`, with the "when NOT to use chat" guidance extended to cover
  async-completion signals.

### Fixed

- **Upload never clobbers a concurrently-created file (codex review P2).**
  The `upload` write replaced a check-then-write (`exists()` then
  `os.replace`/`rename` — which could overwrite a file created in the race
  window) with an atomic hard-link into place that fails with an "already
  exists" error instead. Fixed in both the Rust companion
  (`fsutil::write_new`) and the Python bridge (`os.link`).
- **`compare` rejects duplicate variant `value`s (codex review P2).** Two
  variants sharing a `value` collided as the keyed-`{#each}` key and as the
  returned `selected`; `validate_spec` now rejects duplicates before render,
  covering both the native and remote (Python) paths.
- **`annotated_image` scales the image into `max_height` instead of clipping
  it (codex review P2).** The height cap now applies to the image itself, not
  just the overflow-hidden stage, so the whole image stays visible and click
  coordinates normalize correctly (a click near the visible bottom previously
  mapped to the wrong `y`).
- **Reconciled skill.md drift between the two shipped copies + added a CI
  drift guard.** The canonical agent skill (`docs/skill.md`, embedded in
  the native Rust MCP server) and the copy shipped with the Python bridge
  (`python/src/aiui_mcp/skill.md`, used by remote SSH hosts via `uvx`) had
  drifted: the Python copy was missing whole sections for capabilities that
  **both** bridges expose — the `compare` tool, the `annotated_image` form
  field, and the `Image sources` (`src`/`thumbnail`) resolution rules plus
  the `data:`-URL shell-encoding anti-pattern. Those sections are now
  present in the Python copy (worded for the bridge's host — local file
  paths resolve on the agent's host, the remote for an SSH session), and
  the `compare` tool is reflected in the intro list, the tool-choice table,
  and the window-size section. Deliberate per-bridge tailoring (frontmatter
  description, intro wording, the terser Python prose, the `width`/`height`
  size overrides) is preserved — this is a targeted reconcile, not a
  flattening. A new `scripts/check-skill-drift.sh` compares the field/tool
  section headers of the two copies and fails if either documents a
  field/tool the other doesn't (with a small, commented allowlist for
  intentional divergences); it runs as the lightweight `skill-drift` job in
  CI so the two copies can't silently diverge again.

## [0.8.3] — 2026-07-29

### Added

- **macOS releases moved to CI (`release-macos.yml`).** The full release
  pipeline — Tauri build, Developer-ID codesign, notarization + stapling
  (App Store Connect API key), DMG, signed updater bundle +
  `latest.json`, tag + GitHub release, PyPI publish — now runs as a
  manually dispatched GitHub Actions workflow on a macOS runner, ported
  from `scripts/release.sh`. The script stays as documented emergency
  fallback; signing material lives in Actions secrets, not in a local
  keychain. Inputs: `version` (sync-checked against all three manifests
  before any build minute is spent), `prerelease` (validate-first flow),
  `publish-pypi`.
- **Forward-compat guard for the MCP 2026-07-28 spec.** The new stateless
  spec retires the `initialize` handshake; modern clients probe a stdio
  server with `server/discover` first and fall back to `initialize` on
  any non-modern JSON-RPC error. Both aiui servers already answer that
  probe correctly (Rust: `-32601`, Python/FastMCP: `-32602`) — but
  nothing pinned the behavior down. Now covered by unit tests on the
  Rust dispatcher (probe → `-32601`; `initialize` → protocol version
  `2025-06-18` + non-empty `instructions`) and a CI smoke assertion
  against the built Python wheel, so a refactor can't silently break
  the fallback signal modern clients rely on.

### Changed

- **Python `mcp` dependency capped at `<2`.** The MCP spec release
  2026-07-28 makes the protocol stateless and comes with substantially
  reworked SDKs ("SDK v2" / client-server split). Remote hosts install
  `aiui-mcp` via `uvx aiui-mcp==<version>`, which re-resolves the open
  `mcp>=1.26.0` dependency on every cold start — so a breaking 2.x SDK
  release would have taken down every remote session without any change
  on our side. The pin (`mcp>=1.26.0,<2`) freezes the remote path on the
  1.x line until aiui migrates to the new spec deliberately.

### Fixed

- **Removing an unreachable remote reported red failures instead of
  succeeding.** Deleting a host (or running a full uninstall) fires three
  ssh-based cleanup steps — remote token, `~/.claude.json` entry, remote
  skill. When the host is unreachable — which is often *exactly why* it's
  being removed, e.g. its key no longer authenticates — each of those
  steps failed with `Permission denied (publickey)` and surfaced as a red
  error line, even though the local deregistration (ssh forward +
  `remotes.json`) had already succeeded and the remote files are
  untouchable anyway. Removal now probes reachability once
  (`host_reachable`, a `BatchMode` ssh that treats exit 255 as
  unreachable) and, for a dead host, emits a single calm "lokal
  deregistriert, Remote-Cleanup übersprungen" line naming what was left
  behind — instead of three failures for files aiui can't reach. Also
  fixed a latent dishonesty in the skill-removal step, which used to
  report green regardless of the ssh exit status; gated behind the same
  probe, it now reports a real failure only when a *reachable* host's
  removal genuinely fails.
- **Contact address corrected in package metadata and SECURITY.md.**
  Both carried a non-existent alias as author/security contact; they now
  point to the real byte5 address. Wheels published before 0.8.3 keep
  the old author field — PyPI metadata is immutable per release.

## [0.8.2] — 2026-06-08

Targeted Cowork cold-start fix. (CHANGELOG entries for v0.5.0 – v0.8.1
live with the release notes on GitHub for those tags; the file resumes
here.)

### Fixed

- **Cowork: "MCP aiui Server disconnected" on every cold start.** When
  Cowork (or any Claude Code client) spawned a fresh `aiui --mcp-stdio`
  and no GUI was already running, that child cold-started the GUI —
  and the freshly-launched GUI then ran `kill_mcp_stdio_started_before_self`
  (v0.4.43) and SIGTERM'd its own bootstrapper. The bootstrapper carried
  the assumption "Claude Desktop will respawn them" right in its trace
  message; Cowork / Claude Code don't respawn, so the user's MCP
  connection died on the first call of every cold start. (2026-06-08
  trace: `housekeeping: killing pre-GUI mcp-stdio child pid=2483 …
  cutoff=…`, 138 ms after the bootstrapper attached.) The pre-GUI sweep
  is now **orphan-gated** the same way `kill_orphaned_mcp_stdio_children`
  (v0.4.46, Bug A) handled the sibling-kill: it still reaps mcp-stdio
  children older than the new GUI, but only when their parent wrapper
  is already gone (`ppid==1` / absent from the snapshot). A live
  bootstrapper has a live parent → spared. Regression tests cover both
  the bootstrapper-spared case and the orphan-reaped case.

## [0.4.46] — 2026-05-29

Dialog-lifecycle hardening. Two field-reported regressions from the
lifecycle work of the last days, both root-caused from a Cowork session's
logs + trace, plus a defensive pass so a malformed or stranded dialog can
never leave the user staring at a dead window.

### Fixed

- **Starting a new Cowork session no longer disconnects aiui in the
  other sessions (Bug A).** `kill_sibling_mcp_stdio_with_same_grandparent`
  (0.4.42) reaped any older `aiui --mcp-stdio` sharing our Claude.app
  *grandparent*. That was meant to clear a Claude-Desktop duplicate-child
  glitch — but in Cowork every concurrently-open session's mcp-stdio sits
  under the *one* Claude.app grandparent, so opening a new session
  SIGTERM'd the still-live aiui child of every other session (the
  2026-05-28 "MCP aiui: Server disconnected" toast on the first MCP call
  of a fresh session; confirmed in the trace: `killing older sibling
  pid=30384 (same grandparent=26243)`). Replaced with
  `kill_orphaned_mcp_stdio_children`, which reaps only *orphaned* children
  — ones whose parent wrapper has died (reparented to launchd). A live
  parallel session's child has a live parent and is now spared. The
  original duplicate-child case is already covered by stdin-EOF.
- **The dialog window can no longer be left empty and unclosable (Bug B).**
  The 0.4.45 X-close fix routed dialog teardown through a frontend
  `onCloseRequested` handler that called `preventDefault()` and then ran
  the cancel/close itself. If that path failed (empty/stale dialog
  state), the close was prevented but never completed — the window sat
  empty and the red X did nothing (the 2026-05-29 overnight report: a
  blank "aiui" window, 9 ignored X-clicks in the GUI log). Teardown is
  now **authoritative in Rust**: the `/render` handler destroys the
  dialog window (`window.destroy()`, bypassing the CloseRequested →
  frontend round-trip) on *every* terminal outcome — submit, cancel,
  native X, TTL, or channel-drop — and the window-event handler cancels
  any in-flight render directly when the user hits X. The fragile
  frontend `onCloseRequested` is gone.

### Hardened (Bug B+, defensive render flow)

- **Invalid specs are rejected before a window opens.** `/render` now
  validates the spec (top-level `kind ∈ {ask,form,confirm}`, known field
  kinds) up front; a bad spec returns `invalid_spec` with a `detail` +
  `hint` the agent can act on, instead of opening a window that renders
  an "unknown_kind" placeholder. mcp-stdio surfaces that as a clear tool
  error so the agent fixes the call and retries.
- **A dialog window may only exist while a dialog is pending.** A
  belt-and-suspenders `sweep_orphan_dialog_window` runs on app
  re-activation (`Reopen`): if a dialog window is found with an empty
  registry, it's destroyed — so a stranded empty frame self-heals even
  if some future teardown path is missed.
- **Every `/render` still resolves to exactly one terminal outcome** —
  user response, `invalid_spec`, `ui_unreachable`, `busy`, or
  `ttl_expired` — never a silent hang behind an empty window. (The
  ack-contract watchdog and TTL paths from earlier releases are retained;
  this release adds the window-teardown guarantee around them.)

## [0.4.45] — 2026-05-28

Stability release. A chain of vivid real-world incidents over the last
days (aiui dying overnight, the next morning's tool call hanging, a
cancel button the agent never heard) traced back to a small set of
root causes. Fixed together; Codex consulted on the two architectural
calls (headless updater + idle-exit removal).

### Fixed

- **aiui no longer dies overnight (Bug #6).** `STDIN_IDLE_LIMIT` (the
  6 h "no stdin input ⇒ parent likely gone ⇒ self-exit" timer in
  `mcp.rs`) is **removed**. Its assumption was false for the common
  case "the user simply didn't run an agent for 6 h" — every night
  the mcp-stdio self-exited, the lifetime grace timer then tore down
  the GUI 60 s later, and Claude Desktop (which does not auto-respawn
  a disconnected MCP server) sat at "Server disconnected" until the
  user restarted it. Confirmed firing multiple times per week. The
  genuine "parent gone" signal is stdin-EOF, which is handled cleanly;
  the stale-child accumulation the timer once guarded against is now
  covered three other ways (sibling-kill, periodic
  `disk_version_if_stale`, pre-GUI kill).
- **Auto-update now works headless (Bug #7).** The update check used
  to live only in the frontend (`lifecycle.ts`), so it ran only while
  a window was open — but aiui is headless almost all the time, so
  auto-updates effectively never fired and users stayed weeks behind
  (the reason the 0.4.42→0.4.44 hops never installed themselves). A
  new Rust-side task in `run()` polls `app.updater().check()` every
  6 h regardless of window state, records any newer release in the
  `PendingUpdate` state, and broadcasts `update:available`. The
  Settings banner (0.4.44) then offers a one-click install. No
  auto-install, no restart under a live dialog.
- **X-closing a dialog now cancels it (Bug #1).** Closing the dialog
  window with the native red X (or ⌘W) fired `WindowEvent::CloseRequested`,
  but the Rust handler returned without emitting `dialog_cancel` — so
  the pending `/render` hung until `DIALOG_TTL` (2 h since 0.4.41) and
  the agent never learned the user dismissed it. A new
  `onCloseRequested` listener in `DialogShell.svelte` emits
  `dialog_cancel` for the in-flight dialog before the window is
  destroyed, exactly like the Cancel button.
- **`/render` TTL-expiry returns a clean cancellation (Bug #5).**
  Previously it returned HTTP 408, which `mcp.rs` surfaced as a
  generic "render http 408" transport error — a different shape than
  a user-driven cancel. Now both paths return the same
  `{cancelled: true}` tool-result shape; only `reason` differs
  (`ttl_expired`).
- **Cancel/submit no longer swallow invoke errors silently (Bug #3).**
  `handleCancel`/`handleSubmit` in `DialogShell.svelte` wrap the
  `dialog_cancel`/`dialog_submit`/`close_window` invokes in try/catch
  with console logging, so a failed round-trip is at least
  diagnosable instead of a silent hang.
- **`COLDSTART_WAIT` raised 8 s → 30 s (Bug #4).** A full GUI cold
  start (Tauri init + WebView + HTTP bind + tunnels) can exceed 8 s on
  a busy Mac; the 2026-05-26 incident showed a tool call dying at the
  8 s mark while the GUI was still coming up. 30 s covers a worst-case
  cold start, still well under any sane client tool-timeout.
- **HTTP bind failure → degraded mode instead of exit-loop (Issue #55).**
  With the process-lifetime lock (0.4.43), a bind failure on :7777 now
  means a *foreign* process holds the port, not a second aiui. Exiting
  in that case released the gui-lock → mcp_attach respawned → bind
  failed again → respawn loop. aiui now stays alive in a degraded
  state: records the error, surfaces the Settings window with the
  explanatory banner (lsof hint included), keeps the lifetime socket
  up. No loop, no silent failure.
- **`kill_remote_mcp_stdio` now validates its host alias (Issue #52
  follow-up).** It was the one ssh call site that relied on the `--`
  end-of-options marker alone, without the `is_valid_host_alias`
  boundary check every sibling helper carries. The alias comes from
  `remotes.json` (already validated at `add_remote` time), so this is
  defense-in-depth rather than a live hole — but it restores the
  "every remote helper validates at its boundary" invariant the rest
  of the hardening pass established.

### Verified / closed

- **Issue #56** (/health ready at exact dialog hard-cap) — already
  fixed by the 0.4.36 single-occupancy refactor (`orphan_count <
  DIALOG_HARD_CAP`, strict). Verified, closeable.
- **Issue #121** (form reappears / restart not recognized, v0.4.37) —
  the Setup-Window respawn-loop, fixed by 0.4.43 ProcessLock + 0.4.44
  ExitRequested veto + this release's idle-exit removal. Verify on the
  reporter's setup, then close.
- **Issue #120** (footer buttons not aligned, content behind, v0.4.37)
  — fixed by the 3-zone shell-layout refactor (PR #129). Verify, close.

### Known / out of scope

- **MCP-client tool-timeout (Bug #2)** — when the client (Claude
  Desktop / Cowork) sends no `progressToken`, our keep-alive
  notifications can't fire, and the client gives up after ~4 min if
  the user takes that long on a form. aiui-side this is now far less
  likely to bite (aiui no longer dies, cold start is more tolerant),
  but the residual is client-side and warrants an Anthropic bug
  report, not an aiui change.

## [0.4.44] — 2026-05-26

Two fixes that close the loop on yesterday's structural changes.

### Fixed

- **`RunEvent::ExitRequested` now vetoes when the GUI is still
  needed.** v0.4.43 traced the event and ran `pre_exit_cleanup`, but
  did not actually stop Tauri's default terminator. Result: the GUI
  still died ~18 ms after every dialog submit on macOS — Tauri's
  default behaviour for "last visible window closed" reached
  `ExitRequested` *after* my `on_window_event` returned, and the
  process exited regardless. v0.4.44 calls `api.prevent_exit()`
  whenever `LifetimeStats.child_count > 0` **or** the dialog registry
  has a pending render. The lifetime-grace timer (60 s after the last
  child detaches) remains the only legitimate "everyone's gone,
  really exit" trigger in normal operation. Explicit exits (`app.exit`
  with a code, lifetime-grace-expired) still run cleanup and proceed.

### Added

- **Pending-update banner in Settings (notification-first auto-update).**
  Replaces the v0.4.43 transparent-silent-install with a model the
  user actually notices. The periodic auto-check (now 6 h cadence,
  raised from 30 min) writes the available version into a new
  Rust-side `PendingUpdate` state and broadcasts an
  `update:available` event. Settings.svelte renders a non-modal
  yellow banner at the top of the window: *"aiui v0.4.X ist
  verfügbar — [Installieren]"*. The Install button routes through the
  manual `checkForUpdates({ silent: false })` path so the user always
  sees the native confirmation modal before anything restarts. Banner
  clears automatically once the on-disk version catches up. No more
  mid-dialog interruptions, no more silent installs the user never
  knows about — both regressions from v0.4.39 / v0.4.43 closed.

### Notes

- New Tauri commands: `set_pending_update`, `clear_pending_update`,
  both consumed by `updater.ts`.
- `StatusReport` gains a `pending_update: Option<String>` field so a
  Settings window that opens *after* the auto-check fires still
  picks up the banner via its 2 s status poll.
- 89 unit tests still pass; clippy `-D warnings` clean; svelte-check
  0 errors. Veto logic deliberately not unit-tested as a pure
  function — extracting it from the `RunEvent` closure was more
  invasive than the test value justified.

## [0.4.43] — 2026-05-23

Cascade-eradication release. The 2026-05-23 trace showed 10 GUI process
restarts in 52 s under v0.4.40, with no traced exit reason — the GUI was
dying via a code path I had never instrumented. Codex review (see PR)
identified the gaps. Five interlocking fixes:

### Added

- **`housekeeping::ProcessLock`** — RAII guard around `fs4`'s
  `try_lock_exclusive` (Unix `flock`, Windows `LockFileEx`). Kernel
  releases the lock automatically on process death, so a crashed
  predecessor never leaves a stale lock blocking the next start. Path
  defaults to `<config_dir>/gui.lock`.
- **`housekeeping::kill_mcp_stdio_started_before_self`** — pure-function
  filter + public helper that terminates every `aiui --mcp-stdio` child
  with `start_time` strictly older than the calling process. Used by
  the GUI at startup right after winning the process-lifetime lock, so
  pre-update children get reset to current-binary semantics without
  waiting for the periodic stale-check to fire.
- **Periodic `disk_version_if_stale` in mcp-stdio** — extra tokio task
  alongside `mcp::run_stdio` runs the on-disk-vs-in-memory version
  comparison every 30 s. On mismatch the child exits cleanly, Claude
  Desktop respawns it against the current binary. Closes the v0.4.40
  "children spawned before the update keep their stale RAM forever"
  gap.
- **`is_update_safe_to_install` Tauri command** — single-line gate
  (returns `dialog_state.stats().orphan_count == 0`) consumed by the
  silent updater path so it can install transparently while no dialog
  is open. Replaces the v0.4.39 "too silent" regression that
  effectively disabled the auto-updater entirely.

### Fixed

- **GUI process-lifetime lock at the start of `run()`.** Two GUIs
  spawned in the same millisecond used to race for the lifetime-socket
  bind and the HTTP-port bind. Now only the first arrival proceeds;
  later invocations exit immediately with a traced
  `(gui-lock-busy)` reason. Race-condition between
  `lifetime::gui_serve`'s probe/remove/bind sequence and concurrent
  `http::serve` `SO_REUSEADDR` binds is gone — atomic at the lock,
  not heuristic at each individual bind site.
- **`RunEvent::ExitRequested` now triggers `pre_exit_cleanup`.** Cmd-Q,
  ⌘W on the last visible window, OS shutdown, and the Tauri-updater
  restart all funnel through this event. Until now Tauri's default
  terminator ran past our `pre_exit_cleanup`, leaving ssh-NTR tunnel
  children to launchd as orphans — the **untraced** exit path that
  drove the Phase 1 cascade in the 2026-05-23 trace.
- **Explicit `pre_exit_cleanup` before `app_handle.restart()`** in the
  updater path. Belt-and-braces in case the implicit
  `ExitRequested` emission ever changes; cleanup is idempotent.
- **Silent updater installs transparently when safe.** `silent: true`
  now downloads + installs + relaunches if the dialog window has no
  pending render. If a dialog is open, the install is deferred to the
  next safe auto-check — no more mid-dialog Settings-window pop-ups,
  but also no more "auto-update never ships" regression.

### Notes

- 89 → 96 unit tests (7 new in `housekeeping::tests` covering
  `ProcessLock` exclusivity, `find_pre_gui_mcp_stdio_to_kill` strict
  cutoff, and the various skip paths). All pass; clippy
  `-D warnings` clean; svelte-check 0 errors.

## [0.4.42] — 2026-05-16

### Fixed

- **Sibling `aiui --mcp-stdio` cleanup at child startup.** Claude
  Desktop has been observed (2026-05-16) to occasionally spawn a fresh
  `aiui --mcp-stdio` child without killing the previous one. Both
  children attached to the lifetime socket, both registered their
  prompt list, and Claude Desktop's slash-command routing then
  couldn't decide which child owned `prompts/get` — `/aiui:health`,
  `/aiui:version` etc. failed with "kein erkannter Befehl" even
  though the prompts were correctly listed in the autocomplete.
  Existing housekeeping covered stale-by-binary-path
  (`kill_stale_mcp_stdio_children`) and stale-by-version
  (`disk_version_if_stale`), but not stale-by-duplicate-spawn —
  same path, same version, just two of them. New
  `kill_sibling_mcp_stdio_with_same_grandparent` runs once at every
  mcp-stdio startup: enumerates running `aiui --mcp-stdio`
  processes, picks the ones whose **grandparent** PID matches ours
  (= same Claude Desktop / Claude Code instance, via the
  `disclaimer`/wrapper-process intermediate), and were started
  **before** us, and sends them SIGTERM along with their wrapper.
  Strict newer-kills-older rule prevents two simultaneously-started
  duplicates from tearing each other down. No-op when grandparent
  can't be resolved (terminal-launched mcp-stdio, orphans). Never
  touches the grandparent itself (= Claude Desktop / Claude Code).
  Cross-platform via the existing `sysinfo` snapshot path used by
  the other housekeeping sweeps. Eight unit tests cover same-/
  different-grandparent, newer-skip, unknown-noop, own-pid-skip,
  and chain-break edge cases.

## [0.4.41] — 2026-05-06

### Changed

- **`DIALOG_TTL` raised from 5 min to 2 h.** A 5-min limit silently
  killed any agent dialog the user took longer than that to fill in;
  worst case the form returned `{cancelled: true, reason:
  "ttl_expired"}` to the agent and the user's input was discarded
  with no warning. 2 h covers any realistic form-filling session
  without artificial pressure and is short enough to auto-recover
  from a genuinely stuck WebView. Reported 2026-05-06 on the Weekly
  Planner form.
- **mcp-stdio `/render` HTTP client now per-call long-poll.** The
  shared reqwest client keeps its 300-s default for /health,
  /version, /update, etc., but POST /render gets an explicit
  `.timeout(DIALOG_TTL + 60 s)` override so it doesn't trip the
  300-s default during a long form. Without the override, every
  reqwest timeout would race the backend TTL sweep and randomly
  return a transport error instead of a clean cancellation.

### Added

- **Backend → frontend TTL handoff.** `DialogRequest` now carries
  `ttl_secs`. Single source of truth for the deadline lives in
  `DIALOG_TTL`; the frontend just reads what the backend declared.
- **Countdown warning banners in the dialog window.** Two-stage:
  - **T-15 min**: yellow, dismissible, *"Noch ca. {countdown} Min
    zum Abschicken — danach werden die Eingaben automatisch
    verworfen."*
  - **T-2 min**: red, non-dismissible, *"Weniger als {countdown}
    bis zum automatischen Abbruch. Bitte jetzt abschicken oder
    abbrechen."*
  - **T-5 s**: frontend auto-cancels via the existing `handleCancel`
    code path, so the user's session ends with a clean close instead
    of the backend's TTL sweep racing a last-second submit.
- **Timers are per-dialog with full reset on every new
  `dialog:show`.** A second dialog after the first submitted starts
  with fresh banners + countdown; callbacks scheduled by the
  previous dialog see a mismatched id and no-op, eliminating
  stale-timer races. Cleanup also runs on submit, cancel, and
  component-destroy.

## [0.4.40] — 2026-05-06

### Added

- **Per-spec dialog window sizing.** `dialog::estimate_dialog_size`
  scans the rendered spec and picks an inner-size in (520..=1100,
  480..=900) before the window surfaces. Wide widgets (`wireframe` ≥ 3
  cols, `mermaid`, `table` ≥ 4 cols, `image_grid` ≥ 4 cols) widen the
  window; long forms grow vertically; tabs add a tab-bar header
  height. Confirm/ask specs keep the 520×480 base. Reused windows
  resize to fit the new spec on each render — a confirm after a long
  form no longer keeps the form's tall geometry. Pure function with
  unit tests.
- **Resizable dialog window.** `resizable(true)`,
  `min_inner_size(360, 320)`, `max_inner_size` removed. The user has
  the last word; the per-spec estimate is just a sensible starting
  point.
- **MCP `notifications/progress` for blocking tool calls.** While
  `confirm`/`ask`/`form` waits on the user, the companion sends a
  progress notification every 10 s with the client-supplied
  `progressToken`. Keeps Claude Desktop and Claude Code from
  concluding "tool hung" mid-dialog (default client timeouts are
  60–120 s). Routed through a new mpsc-based stdout writer in
  `mcp::run_stdio` so notifications interleave cleanly with the
  eventual tool response. No-op if the client doesn't pass a
  `progressToken`. v0.4.40.
- **Tool descriptions explicitly state blocking semantics.** The
  `confirm`/`ask`/`form` descriptions now end with a note that the
  tool blocks until user submit/cancel, that responses can take
  minutes, and that progress notifications fire every ~10 s. Stops
  the agent from typing "Companion scheint zu hängen" when the
  user is just thinking.

## [0.4.39] — 2026-05-06

### Fixed

- **Silent update-check no longer steals focus mid-dialog.**
  `checkForUpdates({ silent: true })` is fired automatically by
  `App.svelte` after every successful render (the `update:check`
  event from Rust), on window focus, and on mount. When a new
  version *was* actually available, the function silently called
  `invoke("surface_for_dialog")` and `ask("Update verfügbar?")`
  anyway — surfacing the Settings window in Regular activation
  policy and stealing focus from whatever dialog the agent had
  on screen. Reproduced 2026-05-06 right after 0.4.38 shipped:
  user opened an agent dialog, the post-render auto-check picked
  up 0.4.38, the install prompt opened on top of the active
  dialog, the user's interaction broke. The silent path now
  truly silent — error toasts, "you're on latest" messages, and
  the install prompt are all gated behind `!silent`. Available
  updates surface only on the next manual "Nach Updates suchen"
  click or on the next GUI restart (Tauri-Updater pulls
  automatically). Console logs the deferred update for
  diagnostics.

## [0.4.38] — 2026-05-06

### Fixed

- **Mermaid flowchart node labels are visible again.** Mermaid 11
  renders flowchart node labels inside `<foreignObject>` SVG elements
  (HTML-in-SVG, for proper word-wrapping). The DOMPurify pass in
  `MermaidView.svelte` was stripping `<foreignObject>` outright as a
  belt-and-braces measure on top of `securityLevel: 'strict'` — the
  result was correctly-shaped, correctly-coloured boxes with **no
  text**. Reproduced 2026-05-06 with a Bundesrepublik-Verfassungsorgane
  flowchart. The forbid-list now keeps only `<script>` (event-handler
  attributes are still caught by `FORBID_ATTR` and the svg/svgFilters
  profile), and Mermaid's native HTML labels render through.

## [0.4.37] — 2026-05-05

### Fixed

- **Orphan ssh-NTR tunnels no longer block fresh aiui starts.** When an
  earlier aiui exited via `app.exit()` or `process::exit()` (multi-
  instance race, lifetime-grace timeout, HTTP-bind failure, uninstall),
  Rust Drop was skipped and `tokio::process::Child::kill_on_drop` never
  fired — `tunnel.rs`'s `ssh -NTR 7777:localhost:7777 ...` children
  re-parented to launchd (ppid=1) and survived. With `ServerAlive`
  keeping their SSH session healthy, they held the remote-side port
  indefinitely. Every fresh aiui then failed `-NTR` with
  `ExitOnForwardFailure` and fell back into "shared forward" mode
  forever, even though *its* parent was the only legitimate tunnel
  owner. Two new mechanisms close this:
  - **Pre-exit cleanup.** Every `app.exit()` / `process::exit()` in
    the GUI now calls `housekeeping::pre_exit_cleanup` first, which
    finds and signals our own ssh-NTR children before the process
    departs. Trace logs each exit with a reason string so the killer
    path is identifiable in `aiui-trace.log` next time something
    goes wrong.
  - **Startup orphan sweep.** GUI startup signals every ssh-NTR-on-
    `cfg.http_port` process whose ppid is 1 — orphans inherited from
    a previously-crashed aiui. Filter is tight (exact arg shape, no
    heuristic `pkill -f`) so unrelated SSH sessions are never
    touched.
- **Trace at every exit path.** `quit_app/uninstall`,
  `setup-close-no-children`, `http-bind-error`, `multi-instance-live`,
  `multi-instance-bind-race`, `multi-instance-pipe-busy`,
  `multi-instance-pipe-race`, `pipe-rotate-failed`, and
  `grace-expired` are now individually labeled in the trace. The
  v0.4.36 respawn-loop investigation was crippled by the absence of
  exit-path attribution; this is the foundation for diagnosing the
  next one.

## [0.4.36] — 2026-05-04

### Fixed

- **Concurrent renders now reject with a 409 instead of evicting the
  in-flight dialog.** `DialogState::register` ran a sweep + hard-cap
  eviction before inserting, so a second `/render` call while a user
  was still answering the first dialog would push the first entry's
  oneshot to "evicted" *while the user was still looking at it* — and
  the new dialog overlaid the same single window. That manifested as
  a confused user, an evict-cancelled prior call, and a second
  unexplained dialog. The new `try_register` rejects with a `BusyInfo`
  carrying `pending_count` + `oldest_age_secs`; `/render` returns
  `409 Conflict` with that body; `mcp.rs` translates the 409 into a
  structured tool-call result that lists the three realistic causes
  (multi-call-per-turn, stale window, parallel session) with
  retry-vs-tell-user guidance for each. The companion now serves at
  most one dialog at a time, structurally.
- **Dialog submit no longer kills the GUI process.** A successful
  form submit destroyed the dialog window via `close_window`, which
  triggered the multi-window `CloseRequested` handler from 0.4.25.
  That handler quit the app whenever no other window was visible —
  and after a typical agent flow the setup window is closed, so the
  dialog window *was* the last visible window. Result: the GUI died
  ~20 ms after `got response` (trace 2026-05-04 16:11:42.197 "GUI is
  gone, will relaunch"). The follow-up tool call then sat 8 s in
  `wait_for_aiui` waiting for the auto-resurrect path to bring the
  HTTP server back, and frequently timed out before that succeeded.
  The dialog window is now *never* a quit trigger — it is a
  per-call ephemeral surface, recreated on demand. The setup window
  retains the original quit behaviour but additionally checks the
  attached-MCP-children counter, so the GUI stays alive headless as
  long as any MCP-stdio child is connected over the lifetime
  socket.
- **`aiui not reachable` tool-call response now diagnostic.** When
  `wait_for_aiui` times out, the response previously said only "not
  reachable on localhost:7777, open aiui from /Applications" —
  unhelpful for the dominant real causes (multiple aiui calls in
  one assistant turn, stale dialog window from a prior session,
  parallel Claude session holding the dialog). The agent then
  relayed that text verbatim and the user re-opened an aiui that
  was already running. The new message lists the four realistic
  causes in priority order, hints at retry-vs-tell-the-user for
  each, and detects local-vs-remote-host context so the SSH-tunnel
  branch only appears on remotes. Phrased as agent-facing guidance
  with an explicit "do not relay this verbatim" instruction.

## [0.4.35] — 2026-05-04

### Fixed

- **Settings window's "Add remote" input no longer hides behind the
  footer.** The 0.4.34 footer-overlap fix bumped `.container`'s
  `padding-bottom` to 75 px globally, plus matching negative
  `margin-bottom` on `.footer`. That was correct for Form.svelte
  (which has many fields and benefits from the reservation), but
  broke Settings.svelte: the "Neuen Remote hinzufügen" input
  section sits right above the footer there and got pushed under
  it. Reverted both globals to the original values (16 / -16 px)
  and moved the height-reservation **into** Form.svelte as a local
  `.form-footer-spacer` div placed immediately before the footer.
  Settings stays untouched, Form keeps its scroll-end clearance.
- **Resync-button tooltip in plain language.** The 0.4.34 string
  ("Pin neu setzen + alte aiui-mcp-Subprozesse killen") was
  implementation jargon. Now it's user-facing wording: "aiui-
  Verbindung zu diesem Host erneuern" / "Refresh aiui connection
  on this host". The button still does the same patch + kill
  sequence; the user just doesn't have to know that.

## [0.4.34] — 2026-05-04

### Added

- **Per-remote resync button in Settings.** Each registered remote
  now has a `⟳` icon button next to "Entfernen". Clicking it
  triggers the same `patch_claude_code_config_remote` +
  `kill_remote_mcp_stdio` sequence that runs in the background at
  every aiui-app startup — but on demand, with the StepResult log
  inline. Lets the user retry a sync that failed silently in the
  background (e.g. the 2026-05-04 `dev@devhost: sweep failed` case)
  without having to close + reopen aiui-app. Sweep failures appear
  in the activity log immediately.

### Fixed

- **Dialog window now opens to the front.** When aiui runs in
  Accessory mode (LSUIElement-style daemon, no Dock icon) macOS
  doesn't bring its windows above other apps even with
  `set_focus()`. An agent rendered a dialog and the user didn't see
  it because Claude Desktop was covering it (reported 2026-05-04).
  `ensure_dialog_window` now temporarily promotes the app to
  Regular activation policy and marks the window
  `always_on_top: true` for the first 800 ms — long enough to win
  against any focused app, short enough that the user can Cmd+Tab
  away naturally afterwards. `close_window` demotes back to
  Accessory once the dialog finishes so we don't grow a permanent
  Dock icon.
- **Form footer no longer covers the last form field.** The
  sticky footer's opaque background overlapped scroll-end content
  because the container's `padding-bottom` didn't reserve the
  footer's height. Bumped from 16 px to 75 px (footer ≈ 61 px +
  buffer) with matching `margin-bottom: -75px` on the footer so it
  still sits flush with the window edge. Visible in tester's
  wireframe-test screenshot 2026-05-04.

### Known issues

- **Cold-start race on the second dialog after `close_window`** —
  the first ask after a `confirm` returned `ReadError` (connection
  reset mid-render); the immediate retry succeeded. Window-Ready
  handshake from 0.4.30 doesn't fully cover the rebuild path. Not
  patched in 0.4.34 because the trace doesn't yet show whether the
  reset originates in the WebView rebuild, the SSH-tunnel layer,
  or a remaining multi-instance edge — symptom-fix without root
  diagnosis would risk masking it instead of fixing it. Tracked
  for 0.4.35 once we have a reproduction with detailed render-path
  trace.

## [0.4.33] — 2026-05-04

### Fixed

- **Multi-instance race that produced empty-message connection
  resets is structurally tied off.** Trace from 2026-05-04 13:06
  showed two aiui-app processes alive at the same time: one bound
  the lifetime socket + HTTP port :7777, the other competed for
  every `ssh -NTR` and fell into shared-forward mode talking to
  itself. Tool calls landed on whichever happened to answer first;
  on every transition the in-flight one reset its connection
  (`RemoteProtocolError("")`). The 0.4.31 fix made that error
  diagnosable; this fix makes it not happen.

  Three structural changes, none of them quick-wins:

  1. **`/probe` carries identity now.** Response includes our pid
     and `AIUI_GIT_SHA` alongside `aiui: true`. The
     tunnel-manager's strict-match decision (new
     `probe_response_is_self`) treats *any* response that doesn't
     carry our own pid + sha as foreign — even if it's a valid
     aiui that just happens to share our token. Foreign responses
     no longer trigger the shared-forward poll mode that hijacked
     tool calls onto the wrong instance.
  2. **Lifetime socket is single-instance-strict.** When the GUI
     starts up and the socket path already accepts connections
     (= another aiui owns it), we exit cleanly with `app.exit(1)`
     instead of `remove_file`'ing the live owner's listener out
     from under it. Stale leftovers (post-crash sockets that don't
     accept) are detected by a connect-probe and only then
     removed. The probe + bind path also exits on bind-failure
     (race-condition coverage). Mirrors the v0.4.25 hardening that
     already exists for HTTP :7777.
  3. **Tunnel-manager dedup confirmed.** `TunnelManager::ensure`
     was already idempotent per host (HashMap key check); reviewed
     and verified in this fix to confirm there's no second path
     for spawning duplicate `ssh -NTR` tasks.

  Six new unit tests cover `probe_response_is_self`: self-match,
  pid-mismatch, sha-mismatch, legacy responses without pid/sha,
  `aiui: false`, invalid JSON. All 58 lib tests green.

## [0.4.32] — 2026-05-04

### Added

- **`wireframe` form-field for UI-layout mockups.** Sibling of
  `mermaid` in the inline-context category — but where `mermaid`
  covers graph-shaped diagrams (flowcharts, sequence/state, gantt),
  `wireframe` covers fixed-position panel grids (dashboard tiles,
  hardware-UI mockups, login-screen sketches). The agent ships a
  declarative spec, aiui renders real CSS-Grid panels with proper
  borders, monospace content, and theme-matched colours instead of
  expecting the model to draw boxes-and-pipes in ASCII:

  ```json
  {
    "kind": "wireframe",
    "columns": 3,
    "panels": [
      {"title": "STATUS",  "col_span": 1, "content": "Tiefe: 18 m\nKurs: 270°"},
      {"title": "EMPFANG", "col_span": 2, "content": "14:32 [STARK]…"},
      {"title": "AKTION",  "col_span": 3, "content": "[T]auchen [A]uf", "tone": "highlight"}
    ]
  }
  ```

  Each panel: optional `title` (uppercase header), `content`
  (multi-line monospace text), `col_span` / `row_span` (default 1),
  `tone` ∈ `{"default", "muted", "highlight"}`. Top-level `columns`,
  `gap`, `label`, `max_height`. Read-only, sits between input fields
  like the other inline-context blocks. Tool-description in `mcp.rs`
  and skill-doc (`docs/skill.md` + bundled `python/aiui_mcp/skill.md`)
  call this out explicitly so the agent stops reaching for ASCII when
  the user asks for a UI mockup.

  *Why this and not `mermaid`*: mermaid's `block-beta` is for
  architecture diagrams, not UI layouts; positioning is graph-driven,
  not grid-driven. Forcing a UI mockup through it produces something
  that doesn't look like a UI. The wireframe field stays small and
  agent-friendly: a panel array on a CSS-Grid is enough for the 90 %
  case (mockup discussions, layout reviews, hardware-UI sketches),
  and v2 / Canvas-mode picks up where this stops (interactive,
  multi-turn, persistent surfaces).

## [0.4.31] — 2026-05-04

### Fixed

- **`aiui_health`, `version`, `update` no longer return empty error strings.**
  When aiui.app on the Mac crashed mid-response or a stale SSH reverse-tunnel
  held :7777 with no live process behind it, `httpx` raised
  `RemoteProtocolError("")` — `str(e)` is empty for that class, so the tool
  responses came back as `{"ok": false, "error": ""}` (and `version` /
  `update` surfaced as bare `Error executing tool …:`). Useless in exactly
  the moment the user needed the diagnostic. Fix is two-part:
  - New `_explain_exc` helper falls back to the exception class name when
    `str(e)` is empty or whitespace-only — guarantees the user always sees
    *something* concrete.
  - `_preflight` (render-path), `version`, and `update` get explicit `except`
    branches for `httpx.RemoteProtocolError` (with restart-aiui guidance) and
    a catch-all `httpx.HTTPError` (so stranger transport errors no longer
    bubble up as bare exceptions). 8 regression tests in
    `tests/test_health_error_handling.py` lock the contract in.

## [0.4.30] — 2026-05-03

### Fixed

- **Window can finally be moved.** Replaced the previous setup of
  `titleBarStyle: Overlay` + `hiddenTitle: true` + `data-tauri-drag-region`
  overlay + `-webkit-app-region: drag` CSS — that combination was
  unreliable on Tauri 2 + WKWebView (macOS 26): the Tauri attribute
  drops mousedown depending on z-order, and `-webkit-app-region` is a
  Chromium-only CSS property that WKWebView simply ignores. The
  fix is structural: drop the overlay setup, use the standard native
  visible title bar that macOS owns and drags itself. Slightly less
  flush look, but a window the user can actually grab and reposition.
  Tester reported this regression three times across 0.4.25 → 0.4.28;
  this finally resolves it.
- **First-render-of-session race resolved.** When the dialog window
  was built fresh (first tool call of a session), the backend used
  to emit `dialog:show` before the Svelte frontend had mounted +
  registered its listeners — the event was lost, the 500 ms ack
  timeout fired, the WebView reloaded, and depending on timing the
  user could end up with a blank window for the entire 2-minute
  Claude-Desktop tool-timeout. Reproduction in trace from
  2026-05-03 18:05.

  Window-ready handshake added: the dialog window's frontend
  signals via a new `dialog_window_ready` Tauri command once both
  `dialog:show` and `ui:ping` listeners are installed. The render
  path waits on a `tokio::sync::watch<bool>` for that signal
  *before* emitting (3 s timeout, falls back to existing ack
  contract if the signal doesn't arrive). The reload-recovery path
  resets the flag and re-waits after the WebView reload, so the
  same race can't reappear after a recovery cycle.

### Internal

- `App.svelte` no longer carries the manual drag-region overlay.
- `Settings.svelte` no longer carries `data-tauri-drag-region` /
  `-webkit-app-region` styles on the header. Native title bar
  handles dragging.
- `app.css` `.container` top-padding reduced from 44 px to 14 px
  (no overlay title bar to clear anymore).
- New Tauri command `dialog_window_ready` and shared
  `Arc<tokio::sync::watch::Sender<bool>>` for the handshake.

## [0.4.29] — 2026-05-03

### Fixed

- **Remote `aiui-mcp` versions are kept in lockstep with the local
  companion.** Until now, the companion's setup wrote
  `args: ["aiui-mcp"]` into the remote's `~/.claude.json` without a
  version pin. uvx then cached whatever version of `aiui-mcp` happened
  to be installed first, indefinitely — no upgrade ever, even after
  the local aiui app was updated dozens of times. This is what
  produced the 2026-04-30 incident: a v0.4.27 companion talking to a
  v0.3.1 mcp-stdio on `customer@macmini` because the pin was missing
  and uvx clung to the originally-cached version. Slash-commands
  added between 0.3.1 and 0.4.27 (e.g. `teach`) were therefore
  invisible on the remote, even though they were live in the local
  binary.

  The fix is structural and entirely code-driven (no SSH commands the
  user has to run by hand):

  1. **Pin in `~/.claude.json`** — the companion now writes
     `args: ["aiui-mcp==<companion-version>"]`. uvx is forced to
     fetch the exact matching wheel before spawning. The patch script
     is idempotent: if the pin is already correct (steady state), no
     rewrite happens.
  2. **Stale-mcp-stdio sweep** — when the pin is *changed*, the
     companion sends `pkill -f 'aiui-mcp'` to the remote. Any
     in-flight child running the old version is taken down so the
     next tool call respawns cleanly against the new pin. Tool calls
     that arrive in the brief replacement window get answered by the
     soon-to-die child (still works) and the one after lands on the
     fresh version.
  3. **Resync on every aiui-app launch** — the setup phase iterates
     all registered remotes in a non-blocking background task and
     applies (1) + (2) per host. The user sees the setup window
     immediately; the SSH round-trips happen out-of-band. Steady
     state (all pins correct) costs ~200 ms per remote; an actual
     update is ~600 ms per remote with a clean kill-sweep.

  Net effect: every update of aiui.app on the user's Mac propagates
  automatically to every registered remote on the next aiui-app
  launch. No manual `uvx tool upgrade aiui-mcp` over SSH, no
  Claude-Desktop restart instruction, no version drift.

## [0.4.28] — 2026-04-30

### Fixed

- **Window can actually be dragged.** The 0.4.25 attempt at making
  the window movable (`data-tauri-drag-region` overlay) didn't
  produce a working drag in Tauri 2.10 + macOS 26 — and likely
  blocked the native title-bar drag on top of it. The fix uses
  *both* mechanisms: the Tauri attribute for cross-platform
  correctness, and `-webkit-app-region: drag` as the WebKit-native
  fallback that actually works on this OS/runtime combo. Drag-zone
  also extends across the setup window's full header (logo, status
  text, version chip — none of those are click targets), with the
  Skill-repair button opting out via `no-drag`. Tester reproduced
  the failure on 0.4.27; this release fixes it for real.
- **Setup-window logo is genuinely larger now.** The 0.4.27 bump
  to 64×64 didn't visibly enlarge the rendered icon — the selector
  `.app-header img.app-icon` apparently didn't beat whatever was
  squishing the inline image. Switched to `.app-header .app-icon`
  with `display: block` and explicit `min-width`/`min-height`,
  bumped to 80×80, border-radius proportional. The brand mark now
  carries actual visual weight in the chrome.

## [0.4.27] — 2026-04-30

### Added

- **`mermaid` field renders schematic diagrams to SVG.** Drop
  `{kind: "mermaid", source: "graph TD; A --> B"}` into a form and
  aiui runs the Mermaid DSL through `mermaid.render()`, sanitises the
  resulting SVG with DOMPurify, and embeds it inline as an
  inline-context block (sibling of `markdown` / `image`). Covers
  flowcharts, sequence diagrams, state diagrams, gantt, ER, mind-maps,
  pie charts. Closes the gap that has had agents reaching for ASCII
  box-and-arrow art whenever they want to *show* a flow rather than
  describe one — the result in any proportional-font surface is
  predictably awful. `securityLevel: 'strict'` on the Mermaid side
  rejects HTML-in-labels; the DOMPurify pass is the second line of
  defence (no `<script>`, no `<foreignObject>`, no event attrs).

### Changed

- **Setup-Window-Header polish.** The aiui logo doubles in size (32 →
  64 px) to actually carry the brand mark instead of being a thumb-
  nail next to text. The Skill-installed indicator moves out of its
  own full-width row up into the header status stack, sharing the
  dot+label visual with the Claude-Desktop connection line. Repair
  button only appears when the skill is missing — that's the only
  state the user can act on. Less vertical real estate, more density
  for the remotes list below.

### Fixed

- **Stale `mcp-stdio` subprocess detects itself across in-place `.app`
  replacement.** macOS keeps the previous binary's code-pages alive in
  any already-running process even after the on-disk file is replaced
  by the in-app updater or a manual DMG drop. Claude Desktop holds its
  spawned `aiui --mcp-stdio` child alive across the update, so it ends
  up answering tool calls with stale logic. Symptom on 2026-04-30: a
  Form tool call crashed the in-memory binary 2 ms after receipt with
  no stderr output, while a fresh aiui v0.4.26 sat unused on disk.
  The GUI-side sweep couldn't catch this because the executable path
  matched. New behaviour: `run_mcp_stdio_only` reads
  `CFBundleShortVersionString` from the on-disk `Info.plist` two
  directories up from `argv[0]` and compares it with our compile-time
  `CARGO_PKG_VERSION`. Mismatch → `exit(0)`. Claude Desktop sees the
  broken pipe and respawns against the freshly installed binary.
  Self-healing — no user-facing dialog, no doc note, no manual
  Claude-Desktop restart needed after future updates.

## [0.4.26] — 2026-04-29

### Fixed

- **Window content no longer overlaps the macOS traffic-light buttons.**
  The 0.4.25 multi-window refactor accidentally dropped the
  `<main class="container">` wrapper from `App.svelte` — that's the
  element that supplies the 44 px Apple-HIG top padding so content
  starts below the title-bar buttons. Settings.svelte rendered
  straight into the body, so the aiui logo collided with the traffic
  lights. Wrapper and the drag region now live in `App.svelte`,
  shared by both windows; Settings.svelte and DialogShell.svelte
  render directly into it.

## [0.4.25] — 2026-04-29

### Changed

- **Setup window and dialog window are now separate Tauri windows.**
  The single-window-with-two-views design produced a class of bugs
  where the agent's dialog ended up stacked behind the user's open
  settings window — invisible, blocking every subsequent render call
  for the rest of the session. Splitting them eliminates that
  structurally: settings live in a `setup` window, dialogs in a
  `dialog` window, both independently movable and stackable in the
  normal macOS sense. Closing one no longer cancels the other.
- **Windows are draggable from anywhere along the top edge.** Each
  window has an invisible 28 px `data-tauri-drag-region` strip
  layered over the macOS overlay title bar so the user can grab the
  window and move it without aiming at the actual title-bar pixel
  row. Reported by early testers when an agent dialog covered work.

### Fixed

- **HTTP-bind failure now exits the second instance.** Earlier
  versions left a half-zombie aiui running when `localhost:7777` was
  already taken — a window that looked alive but answered no
  requests. Combined with the old single-window design that produced
  the 2026-04-29 hung-dialog incident: a stale instance held the
  pending dialog while a freshly started one masked it with its
  settings view. The new behaviour is to exit(1) on bind failure,
  surface the existing instance via tauri-plugin-single-instance,
  and let mcp_attach's auto-resurrect bring things back cleanly.
- **Window close on the setup window no longer kills an in-flight
  dialog.** The close handler now checks whether any other window is
  still visible before quitting the app. Dialog windows stay alive
  when the user closes settings; the app only quits when the last
  window goes away. Auto-resurrect on the next tool call still works
  the same way.

### Internal

- `surface_main_window` / `reload_main_webview` now operate on the
  `dialog` window label specifically. `dialog:show` and `ui:ping`
  events are routed via `emit_to(DIALOG_WINDOW_LABEL, …)` so the
  setup window never sees them.
- New `DialogShell.svelte` holds the dialog-specific listeners and
  routing; `App.svelte` branches on `getCurrentWebviewWindow().label`
  and renders Settings or DialogShell accordingly.

## [0.4.24] — 2026-04-29

### Changed

- **Persistent TRACE logging for aiui's own modules.** Bumps the
  `aiui_lib::*` log level from Info to Trace by default; everything
  else stays at Info to keep volume manageable. Log rotates at 5 MB
  with one previous file kept — covers a multi-hour session without
  filling up disk. Diagnosed after a 4-minute MCP-timeout on a trivial
  form spec where the existing log gave us nothing because the entire
  render pipeline emits only at Trace level. Next time something
  hangs, the log under `~/Library/Logs/de.byte5.aiui/` will show
  exactly which phase stuck.

## [0.4.23] — 2026-04-28

### Added

- **`confirm` accepts an `image`.** Pass `image: {src, alt?, max_height?}`
  to fold a visual sign-off into a plain yes/no — no more `form`-with-
  one-image-and-two-buttons workaround for "is this generated logo OK?".
  `src` follows the standard aiui resolution rules (data: URL, http(s)
  URL, or absolute / `~/`-rooted local path on the host the agent runs
  on).
- **`ask` options accept a `thumbnail`.** Each option in the array can
  now carry a `thumbnail: <src>` rendered as a 56 × 56 preview to the
  left of the label. Same resolution rules. Closes the gap between
  text-only `ask` and full-blown `image_grid`: 2–6 visual options
  belong here, not in a `form` wrapper.

### Changed

- **Skill catalog calls out the visual variants.** Tool-choice table
  gains explicit rows for "yes/no on a generated image" → `confirm`
  with `image`, and "pick one of 2–6 images" → `ask` with `thumbnail`.
  Removes the implicit pressure to reach for `form` for either case.

## [0.4.22] — 2026-04-28

### Added

- **Image fields accept local filesystem paths.** `src` (and the
  `list.items[].thumbnail` slot) now resolve absolute or `~/`-rooted
  paths on the host where the agent runs — Mac for local Claude Code,
  the remote host for SSH-tunneled remotes. The bridge reads the file,
  base64-encodes it, and inlines it as a `data:` URL before the spec
  leaves the agent's host. The Mac-side WebView only ever sees `data:`
  URLs, so the strict CSP is preserved. 10 MB cap, MIME guessed from
  the extension. Resolves the awkward base64-by-hand or
  `data:`-via-shell-pipeline workflow that agents kept tripping over.
- **Symmetrical Python implementation in `aiui-mcp`.** The Rust bridge
  (used in local Mac sessions) and the Python bridge (used by every
  remote that resolves `uvx aiui-mcp`) share behaviour 1:1 — same
  accepted formats, same caps, same fail-soft semantics. Drift between
  them would produce "works locally, broken on remote" bugs; mirrored
  unit tests on both sides guard against that.

### Changed

- **Skill catalog rewrites the image-source guidance.** "Pick the
  simplest format that works" — path > URL > `data:`. Adds the
  shell-pipeline anti-pattern explicitly (terminal renders the
  `data:image/...` prefix and eats the rest of the call). Calls out
  cross-host paths as a non-resolving case so agents on remotes know
  to use `http(s)://` for Mac-side files.
- **Python CI now runs `pytest`.** New `python/tests/` directory holds
  the local-path resolver tests; CI installs the `dev` extra and runs
  the suite before the build step.
- **Release pipeline publishes the PyPI side in lockstep.**
  `scripts/release.sh` gained a fourth version-sync gate
  (`python/pyproject.toml`), runs `uv build` alongside the Tauri build
  (covered by `--dry`), and runs `uv publish` after the GitHub release.
  Required `UV_PUBLISH_TOKEN` is checked up-front, so a missing token
  fails the run before any Tauri work happens. Background: `aiui-mcp`
  0.4.2 stayed pinned on PyPI long after the Tauri side had moved past
  the `widgets` → `teach` rename, leaving every remote-host install in
  the old prompt world. This closes that gap structurally — next
  release the two sides ship or fail together. (Landed on main between
  0.4.21 and 0.4.22; first release that exercises it is 0.4.22.)

## [0.4.21] — 2026-04-28

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
