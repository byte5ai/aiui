# Contributing to aiui

Thanks for reading this. aiui is a small project; contributions are
welcome as issues, discussions, or pull requests.

## Quick links

- [Open issues](https://github.com/byte5ai/aiui/issues)
- [Widget catalog](docs/skill.md) (how aiui is meant to be used)
- [Changelog](CHANGELOG.md)

## Repository layout

```
aiui/
├── companion/                Tauri companion (Rust + Svelte 5)
│   ├── src-tauri/            Rust backend, HTTP + lifetime + tunnel manager
│   └── src/                  Svelte frontend — settings window + dialog widgets
├── python/                   aiui-mcp PyPI package (FastMCP server)
│   └── src/aiui_mcp/
├── docs/
│   └── skill.md              Agent-facing widget catalog (shipped into
│                             ~/.claude/skills/aiui/)
├── scripts/
│   └── release.sh            Stub that refuses to run — releases are CI-only
├── assets/                   Brand assets (icon, logo, dmg background)
└── CHANGELOG.md
```

## Building locally

Prerequisites: Rust (stable), Node.js ≥ 20, [uv](https://docs.astral.sh/uv/),
plus Xcode command-line tools on macOS.

```sh
cd companion
npm install
npx tauri build --target aarch64-apple-darwin
```

Output: `companion/src-tauri/target/aarch64-apple-darwin/release/bundle/macos/aiui.app`

On Windows, build the NSIS installer the same way CI does — the config
override skips the updater bundle, which needs the signing key:

```sh
cd companion
npm install
npx tauri build --target x86_64-pc-windows-msvc --bundles nsis \
  --config src-tauri/tauri.ci.conf.json
```

A local build like this is fine for development. It is not a release —
see "Releasing" below.

For the Python side:

```sh
cd python
uv build    # produces dist/aiui_mcp-*.whl + .tar.gz
```

## Releasing

**Releases run exclusively in GitHub Actions.** Not locally, not on the
maintainer's machine. All signing material lives in Actions secrets
(`MACOS_*`, `TAURI_SIGNING_PRIVATE_KEY*`, `UV_PUBLISH_TOKEN`) — never in a
local keychain, never in the repo. `scripts/release.sh` is a stub that
refuses to run; it exists only to stop anyone from reinventing a local
build path. If Actions is down, wait for it.

Before dispatching, the bump PR has to do all of this:

- bump the version in all **four** manifests —
  `companion/src-tauri/Cargo.toml`, `companion/src-tauri/tauri.conf.json`,
  `python/pyproject.toml` and `companion/package.json`;
- rename `## [Unreleased]` in `CHANGELOG.md` to `## [X.Y.Z] — <date>`, so
  the version being released has a section.

`scripts/check-release-preconditions.sh` is what enforces it, and it runs
twice: in CI on every PR (no argument — the four manifests must agree with
each other and with a CHANGELOG section), and as the release workflow's
first step after checkout (`… X.Y.Z` — the manifests must additionally equal
the dispatched version, and `vX.Y.Z` must not already be on the remote).
Run it locally before opening the bump PR:

```sh
scripts/check-release-preconditions.sh          # the tree agrees with itself
scripts/check-release-preconditions.sh 0.11.0   # …and with what you'll dispatch
```

Releases are cut **from `main` only** — the workflow's very first step
refuses any other ref — and the run executes the drift guards, `pytest` and
`cargo test --lib` *before* the Developer ID certificate is imported, so a
red test never reaches the signing keychain.

### macOS — `release-macos.yml`

Builds on a `macos-14` runner, Developer-ID-signs + notarizes via the App
Store Connect API key, produces the DMG + signed updater bundle +
`latest.json`, cuts the GitHub release **as a draft**, publishes
`aiui-mcp` to PyPI, and dispatches the Windows workflow.

```sh
gh workflow run release-macos.yml -f version=X.Y.Z --repo byte5ai/aiui

# validate-first (no PyPI, GitHub pre-release):
gh workflow run release-macos.yml -f version=X.Y.Z \
  -f prerelease=true -f publish-pypi=false --repo byte5ai/aiui
```

PyPI runs last, after the GitHub release succeeded, because PyPI versions
are permanent. The tag and release steps are idempotent, so a run that
failed at PyPI can be re-dispatched with the same version to recover — and
since #209 that re-run also **re-uploads** what it just built
(`gh release upload --clobber`), instead of building it and throwing it
away. Its `latest.json` is merged into the published feed rather than
replacing it, so re-dispatching macOS after the Windows run does not drop
the `windows-x86_64` entry.

**The release stays a draft until the Windows run completes.** A draft is
not served by `releases/latest/download/latest.json`, which is what every
installed aiui polls — so until both platforms are in the feed, clients
keep resolving the previous, complete one. This is deliberate: a feed
missing a platform does not mean "no update" for that platform, it means
an *error* (`TargetsNotFound`). v0.10.1 shipped exactly that and broke
every Windows client's update check (#190).

### Windows — `release-windows.yml`

Dispatched **automatically** by the macOS workflow, against the tag it
created. Builds the NSIS installer plus the signed updater bundle,
attaches all three artifacts to the draft release, patches `latest.json`
with the `windows-x86_64` entry, validates the assembled feed with
`scripts/check-updater-feed.sh`, and only then publishes the release.

If that run fails, the release **stays a draft and no client sees the new
version** — including macOS users. That is the intended coupling for a
two-platform product: half a release is what caused #190. Fix the cause
and re-dispatch by hand:

```sh
gh workflow run release-windows.yml -f tag=vX.Y.Z --repo byte5ai/aiui
```

The run is idempotent (`gh release upload --clobber`, and the publish step
skips an already-published release), so repeating it is safe.

The Windows `.exe` ships **unsigned** — no Authenticode certificate, so
SmartScreen warns on first launch. That is a deliberate v1 decision, not
an oversight.

Note that this differs from the per-push CI build: CI uses
`tauri.ci.conf.json` (`createUpdaterArtifacts: false`) and produces the
unsigned `.exe`. The updater signature (`<installer>.exe.sig`) that the
update feed needs comes from `release-windows.yml` alone.

### The updater feed

`scripts/check-updater-feed.sh <latest.json> <tag> <version>` is the guard
that keeps `latest.json` publishable. It refuses a feed that is missing a
shipped platform, carries an empty signature or url, points an entry at a
different release's artifact, or advertises the wrong version. The list of
platforms aiui ships for lives in that script, in one place — adding one
later is a one-line change there.

`scripts/test-check-updater-feed.sh` exercises it against fixtures and runs
on every PR, so the guard cannot quietly stop guarding.

## Issues

The aiui app's Settings window has a „Report issue" button that opens a
prefilled GitHub issue with the current version + build SHA. Use it — it
saves us a round of „which version are you on?".

For bug reports, please include:

- aiui version (visible in Settings as a chip, e.g. `v0.2.0`)
- Your OS and version (macOS or Windows)
- Whether you hit it locally or via a remote host setup
- What you did / expected / saw

## Pull requests

1. Keep changes focused. One PR, one concern.
2. If you're adding a widget, also extend `docs/skill.md` with an anti-
   pattern section — every widget needs guidance for the agent, otherwise
   it degrades into UI slop.
3. New user-facing strings: add them to `companion/src/i18n/de.json` and
   `en.json`, keyed by short stable paths.
4. Run `npm run check` in `companion/` before pushing — catches Svelte /
   TypeScript issues early.
5. If your change would conflict with another PR that is still open — the
   usual case is two changes to the same function — base it on that PR's
   branch instead of `main`:

   ```sh
   gh pr create --base the-other-branch --head your-branch
   ```

   GitHub retargets it at `main` automatically once the parent merges. Say
   in the description which PR it is stacked on and why. CI runs on every
   pull request regardless of base, so a stacked PR is checked like any
   other.

## Design principles

The constraints that shape decisions in this project:

- **User installs nothing per project.** aiui registers itself as a
  global MCP server in Claude Code (`~/.claude.json`) on first launch;
  the PyPI package is pulled on demand via `uvx`.
- **Agents can't make slop.** Rules live both in tool docstrings (always
  visible) and the full skill (auto-installed). Widgets constrain rather
  than expand freedom where that improves outcomes.
- **No ad-hoc web dashboards and apps.** aiui exists to replace the pattern of the
  agent spinning up a temporary local web UI. If a feature request pulls
  in that direction, the answer is usually „more widget primitives" not
  „more escape hatches".
- **Updates are zero-friction.** Every release must flow through the
  in-app updater; no manual zip-swap dance for users.

## License

By contributing, you agree your contribution is licensed under the MIT
license (see [LICENSE](LICENSE)).
