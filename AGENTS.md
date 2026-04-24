# Agent Instructions

These rules apply to all AI agents working on this repository (Claude, Codex, Copilot, etc.).

## Git Workflow
- **Never push directly to `main`.** All changes go through feature branches and pull requests.
- **Branch naming:** `feat/`, `fix/`, `refactor/`, `docs/`, `chore/`, `test/`, `release/`, `dev/` prefixes.
- **Conventional commits:** `feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`, `release:`, `dev:`.
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

## Engineering Standards
This repo's engineering-standards status is tracked in `.github/engineering-standards.yml`.
Source of truth: the account's `engineering-standards` repo.
