#!/usr/bin/env bash
# aiui does NOT release locally — not on CI-less machines, not on anyone's Mac.
#
# Releases run exclusively in GitHub Actions, on a GitHub-hosted macOS
# runner, exactly like every other devhost project (***, ***):
# Developer-ID signing + notarization (App Store Connect API key), DMG +
# signed updater bundle + latest.json, GitHub release, PyPI publish. The
# signing material lives in GitHub Actions secrets — never in a local
# keychain, never on the maintainer's Mac (which is not a build host).
#
# Trigger the release:
#   gh workflow run release-macos.yml -f version=<X.Y.Z> --repo byte5ai/aiui
#   # or: GitHub UI -> Actions -> "Release (macOS)" -> Run workflow
# Validate-first (no PyPI, GitHub pre-release):
#   gh workflow run release-macos.yml -f version=<X.Y.Z> \
#     -f prerelease=true -f publish-pypi=false --repo byte5ai/aiui
#
# This script exists only to stop agents and humans from reinventing a
# local build path. If GitHub Actions is down, wait for it — do not sign
# aiui locally. See CONTRIBUTING.md -> Signing / notarising / releasing.
set -euo pipefail
cat >&2 <<'MSG'
✋ aiui does not release locally.

Releases run in GitHub Actions on a macOS runner. Trigger:
  gh workflow run release-macos.yml -f version=<X.Y.Z> --repo byte5ai/aiui

Validate-first (no PyPI publish, marked as GitHub pre-release):
  gh workflow run release-macos.yml -f version=<X.Y.Z> \
    -f prerelease=true -f publish-pypi=false --repo byte5ai/aiui

Details: CONTRIBUTING.md -> Signing / notarising / releasing
MSG
exit 1
