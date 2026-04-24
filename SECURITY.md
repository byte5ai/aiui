# Security Policy

## Supported Versions

Only the latest `0.2.x` release is supported with security fixes. The
in-app updater delivers patches automatically to installed clients.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Email the details to <cw@byte5.de> with a clear subject line
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
