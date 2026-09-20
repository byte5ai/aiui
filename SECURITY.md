# Security Policy

## Supported Versions

Only the latest released version is supported with security fixes.

aiui checks for a new version automatically every 6 hours and surfaces
what it finds — a system notification and a banner in the settings window.
**Installing is not automatic:** it takes one click on that banner, or
`/aiui:update` in Claude Code, or the „Check for updates" button in
Settings. A security fix therefore reaches a client only once its user
acts, which is worth knowing when you are deciding how long a
vulnerability stays exploitable in the field.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Email the details to <cwendler@byte5.de> with a clear subject line
(`[aiui] security report — <brief summary>`) and include:

- What the vulnerability is and where in the code it lives
- Reproducible steps or proof-of-concept, if you have one
- Affected versions

We aim to acknowledge within 72 hours and publish a fix within 14 days
for high-severity issues.

## Scope

In scope:

- `aiui.app` (Tauri companion) — signing/notarization, HTTP endpoint,
  tunnel manager, lifetime socket, auto-updater trust chain
- `aiui-mcp` Python package — token handling, preflight, render call path
- Release pipeline signing and notarisation

The release workflows pin every third-party action to a full commit SHA
and the Rust compiler to the channel in `rust-toolchain.toml`, so a moved
upstream tag cannot reach the job that holds the Developer ID
certificate, the notary key and the minisign updater key
(`scripts/check-workflow-pins.sh` enforces this on every pull request).
A floating ref that slips past that guard is in scope — please report it.

Out of scope (report upstream instead):

- Vulnerabilities in Tauri, Rust std, WebKit, FastMCP, or uv
- Issues in Claude Desktop itself
- Missing macOS hardening that is Apple's responsibility

## Our commitments

- Releases are signed with the byte5 Developer ID (`VG5X6JCLGF`) and
  notarised by Apple.
- Updater artifacts are signed with an Ed25519 key and verified on the
  client before installation.
- No telemetry, no outbound calls other than the GitHub-hosted updater
  feed.
