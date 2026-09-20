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
  tunnel manager, lifetime socket, auto-updater trust chain,
  render-time image resolver (the `http(s)://` `src` fetch)
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
- No telemetry and no analytics. aiui makes exactly two kinds of
  outbound request: the GitHub-hosted updater feed, and a `GET` for any
  `http(s)://` image URL a dialog spec asks it to render — publicly
  routable destinations only; loopback, private, link-local, CGNAT and
  ULA targets are refused before a socket is opened, resolved addresses
  are pinned so a second DNS answer cannot slip past that check, and
  redirects are not followed. Nothing else leaves your machine.
