# Agent Instructions

These rules apply to all AI agents working on this repository (Claude, Codex, Copilot, etc.).

## When to use aiui (dogfooding)

This repo dogfoods its own product. When you work here with aiui's MCP tools
available — Claude Desktop, or Codex/Claude Code tunnelling to the maintainer's
Mac — prefer a native dialog over asking in chat, the same rule the
[aiui skill](docs/skill.md) gives end users. Defaults:

- **Destructive or hard-to-undo yes/no** (delete, drop, force-push, rollback,
  prod deploy, dispatch a release) → `confirm` with `destructive: true`, even
  if loose approval was already given.
- **Pick one of N options where per-option context matters** (which migration
  path? which release flavour?) → `ask` with a per-option `description`.
- **Multi-field input, a secret/token, a date or bounded number, or a sortable
  ranking** → `form` (use a `password` field for secrets — never paste them in
  chat).
- **A flow/sequence/state you'd otherwise sketch in ASCII** → `form` with a
  `mermaid` field.
- **A completion signal that needs no answer** (tests green, release dispatched,
  blocked on a conflict) while the maintainer may be tabbed away → `notify`.

Chat still wins for content the user reads but doesn't answer — status,
summaries, logs, single free-text replies. [`docs/skill.md`](docs/skill.md) is
the source of truth for the full tool set and field types; keep this section in
sync with it rather than forking the guidance.

## Git Workflow
- **Never push directly to `main`.** All changes go through feature branches and pull requests.
- **Branch naming:** `feat/`, `fix/`, `refactor/`, `docs/`, `chore/`, `test/`, `release/`, `dev/` prefixes.
- **Conventional commits:** `feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`, `dev:`.
- **No `Co-Authored-By:` trailers for Claude or other AI agents.** Commits are made under the configured git identity, with no model-attribution footer.
- **Never force-push** to any shared branch.
- **Never commit secrets** (`.env`, API keys, tokens, credentials).
- **Never skip hooks** (`--no-verify`).

## Pull Requests
- Keep PR titles short (<70 chars), use conventional prefix.
- One logical change per PR.
- Ensure tests pass before requesting merge.

## Pre-push Hook
A `.hooks/pre-push` hook blocks direct pushes to `main`/`master`. Override only when explicitly instructed:
~~~bash
ALLOW_PUSH_TO_MAIN=1 git push origin main
~~~

## Releases
aiui releases run **exclusively in GitHub Actions**, never locally and never on the maintainer's Mac — identical to every other devhost project (zaplex, nexgenvideo). The maintainer's Mac is **not** a build host; devhost sessions trigger the workflow, they do not build.

- **macOS release:** the `Release (macOS)` workflow — `.github/workflows/release-macos.yml`, `workflow_dispatch`. It builds on a `macos-14` runner, Developer-ID-signs + notarizes via the App Store Connect API key, builds the DMG + signed updater bundle + `latest.json`, cuts the GitHub release, and publishes `aiui-mcp` to PyPI. Trigger it:
  ~~~bash
  gh workflow run release-macos.yml -f version=<X.Y.Z> --repo byte5ai/aiui
  # validate-first (no PyPI, GitHub pre-release):
  gh workflow run release-macos.yml -f version=<X.Y.Z> \
    -f prerelease=true -f publish-pypi=false --repo byte5ai/aiui
  ~~~
- **Windows installer:** `release-windows.yml` (`workflow_dispatch`), attached to the release afterwards.
- **Signing material** lives in GitHub Actions secrets (`MACOS_*`, `TAURI_SIGNING_PRIVATE_KEY*`, `UV_PUBLISH_TOKEN`) — never in a local keychain, never in the repo, never in chat.
- **`scripts/release.sh` is not a release path.** It is a stub that refuses to run. Do not build or sign aiui on a local machine, and do not tell anyone to. If GitHub Actions is down, wait for it.

Before dispatching, bump the version in `companion/src-tauri/Cargo.toml`, `companion/src-tauri/tauri.conf.json`, and `python/pyproject.toml` so all three agree with the dispatched version — the workflow's first step hard-fails on drift.

## Engineering Standards
This repo's engineering-standards status is tracked in `.github/engineering-standards.yml`.
Source of truth: the account's `engineering-standards` repo.
