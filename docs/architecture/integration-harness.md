# aiui remote-path integration harness

The bugs that made aiui whack-a-mole (409-storm, stranded empty windows,
ReadError, host dying after submit) all lived in the **timing / integration
layer** — the live interplay of companion ⟷ tunnel ⟷ bridge ⟷ dialog
lifecycle. Pure-function unit tests can't reach that layer, which is exactly
why it stayed unprotected. This harness drives the **real** chain and asserts
its behaviour, so that class of regression is caught before a release instead
of by the user.

Key fact established 2026-05-30: a Claude Code session running on the remote
can reach the real companion's HTTP API over the existing reverse tunnel
(`localhost:7777` + the pushed token). So the harness driver runs **from the
remote**, against a **real** companion on the Mac — not a simulated one.

## Stufe 1 — read-only smoke (DONE)

`python/tests/test_integration_live.py`. Runs from the remote against the live
companion. Strictly read-only (`/ping`, `/health`, `/version`, `/probe`,
401-on-bad-token, `GET /render/{id}` unknown-id) → **no dialog windows pop**.

- Opt-in via `AIUI_LIVE=1`; the normal `pytest` run and CI skip it (no network).
- Skips with a message if no companion is reachable.
- Version-tolerant: `wire_version` and the async `GET /render/{id}` route are
  checked only if present, so it also passes against an older installed release.

```
AIUI_LIVE=1 uv run --extra dev pytest tests/test_integration_live.py -v
```

Verified 2026-05-30: 6 passed against the installed companion (v0.4.45) over
the tunnel.

## Stufe 2 — render path + window lifecycle (DESIGN)

This is where the actual bugs lived. To exercise it automatically we must (a)
complete a render→answer→teardown cycle without a human, (b) observe window
state, and (c) NOT spam the user's screen with real dialogs.

There is deliberately **no** HTTP endpoint to answer a dialog in production (it
would be a UX/security hole). So Stufe 2 needs a small, strictly test-gated
hook in the companion — the "test counterpart":

### Companion test mode (preferred)

Active only when launched with `AIUI_TEST_MODE=1` **and** authenticated with the
token. Absent from normal runs — the routes 404 when the env is unset, so it
can never be reached in production. It adds:

- `POST /test/answer/{id}` — resolve a pending dialog by id with a canned
  `submit{result}` or `cancel`, exactly as the frontend would. Lets the driver
  complete the cycle with no UI automation and no human.
- Test-mode renders the dialog window **hidden / off-screen** (or suppress the
  window entirely, registry-only) so a test sweep doesn't flash dialogs at the
  user.
- `/health` (or a `/test/windows`) reports the **dialog-window labels** so the
  driver can assert "one window per render, torn down after terminal".

### Driver scenarios (pytest, `AIUI_LIVE=1` + companion in test mode)

Mirrors the spec's required scenarios:

- **async render**: `POST /render` (`x-aiui-async`) → 202 `{id}`; `GET
  /render/{id}` → `{pending}`; `POST /test/answer/{id}` → `GET` returns the
  terminal result; assert the window for `id` is gone.
- **no-409 / multi-window**: fire two concurrent `POST /render` → assert **both**
  get 202 `{id}` (single-occupancy gone); assert two distinct windows; answer
  both; assert both torn down.
- **cancellation-safety**: start a render, drop the client connection
  mid-poll → assert the registry slot frees and the window is destroyed (no
  2 h leak / 409 on the next render).
- **TTL / channel-drop / Claude-Desktop-quit / restart** — the remaining spec
  scenarios, each asserting a clean terminal outcome.

### Alternative without a companion change

A Mac-side AppleScript/JXA agent that finds aiui dialog windows, clicks their
buttons, and reports the window count. Works against an unmodified companion,
but is timing-fragile, needs Accessibility permission, and can't suppress the
on-screen flash. The companion test-mode above is cleaner and is the
recommended path; this stays a fallback.

## Preconditions & honest scope

- **To validate *this* PR's code, the v0.5.0 build must run on the Mac.** The
  driver tests the companion that's installed; against the current release
  (v0.4.45) Stufe-2 assertions for new behaviour (async, no-409) don't apply.
- Stufe 2 is a real build (the test-mode hook in the companion + the driver
  suite). It is **not** wired into ordinary CI, which has no real Mac+remote
  pair — it's a pre-release check run against a real build, or a dedicated rig.
- The test-mode hook must be reviewed to confirm it cannot activate in
  production (env-gated at launch, token-gated, routes absent otherwise).
