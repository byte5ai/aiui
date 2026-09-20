#!/usr/bin/env bash
#
# check-release-preconditions.sh — everything that must hold before a
# release is cut (#209).
#
# WHAT THIS GUARDS
# ----------------
# `release-macos.yml` is the only path that tags, Developer-ID-signs,
# notarizes and publishes aiui, and a PyPI version is permanent. Before it
# spends twenty minutes of a 10×-billed macOS runner on that, four
# manifests have to agree with each other and with the dispatched version,
# the CHANGELOG has to have a section for it, and the tag must not already
# exist.
#
# The four manifests:
#
#   companion/src-tauri/Cargo.toml       the build
#   companion/src-tauri/tauri.conf.json  Info.plist / CFBundleShortVersionString
#   python/pyproject.toml                the PyPI artifact every remote pins
#   companion/package.json               npm metadata a contributor reads
#
# The first three drifting produced #82 (updater confusion) and the
# v0.4.2/v0.4.21 Tauri-vs-PyPI split. The fourth was left out of the old
# inline check and had drifted six minor versions (0.4.5 against 0.10.1)
# before anyone noticed — which is the whole argument for enumerating them
# in one list instead of hand-writing a fourth `grep`. A fifth manifest is
# one line in MANIFESTS below.
#
# The CHANGELOG assertion exists because the file had drifted in BOTH
# directions: dated, released-looking headings for versions that were never
# tagged, and ten tags in v0.5.0…v0.8.1 with no section at all. A user who
# reads "v0.9.0 added the upload tool" and cannot find v0.9.0 opens an
# issue; a remote pinning `aiui-mcp==0.9.0` from the CHANGELOG points at a
# PyPI version that does not exist.
#
# TWO MODES
# ---------
#   check-release-preconditions.sh
#       The four manifests agree with each other, and CHANGELOG.md has a
#       `## [<that version>]` section. Needs no dispatch input and no
#       network, so CI runs it on every PR — drift is caught in the bump
#       PR, not at dispatch time.
#
#   check-release-preconditions.sh <version>
#       Additionally: all four equal <version>, and `refs/tags/v<version>`
#       is absent ON THE REMOTE. The remote is the only place that can be
#       asked — `actions/checkout@v4` fetches depth 1 and no tags, so the
#       `git rev-parse "$TAG"` this replaces could never fire. Do not
#       "fix" that by setting `fetch-depth: 0`; that pulls the full
#       history onto a macOS runner for one ref lookup.
#
# Run locally:  scripts/check-release-preconditions.sh
#               scripts/check-release-preconditions.sh 0.11.0
# CI:           .github/workflows/ci.yml -> job `release-preconditions`
#               .github/workflows/release-macos.yml -> `Check version sync`
# Self-test:    scripts/test-check-release-preconditions.sh

set -euo pipefail

# Resolve repo root from this script's location so it works from anywhere.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

WANT="${1:-}"

fail() {
  echo "release-preconditions: FAIL — $*" >&2
  exit 1
}

read_toml_version() { # first `version = "…"` at line start
  grep -E '^version = ' "$1" | head -1 | cut -d'"' -f2
}

read_json_version() { # the top-level "version" key
  python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$1"
}

# path:reader — the single list every version-carrying manifest belongs in.
MANIFESTS=(
  "companion/src-tauri/Cargo.toml:read_toml_version"
  "companion/src-tauri/tauri.conf.json:read_json_version"
  "python/pyproject.toml:read_toml_version"
  "companion/package.json:read_json_version"
)

paths=()
versions=()
for entry in "${MANIFESTS[@]}"; do
  path="${entry%%:*}"
  reader="${entry##*:}"
  [ -f "$path" ] || fail "$path does not exist (run this from a full checkout)"
  version="$("$reader" "$path")" || fail "could not read a version out of $path"
  [ -n "$version" ] || fail "$path carries no version"
  paths+=("$path")
  versions+=("$version")
done

# The first manifest is the reference; every other one is compared to it, so
# the message always names the drifted file and both versions.
ref="${versions[0]}"
ref_path="${paths[0]}"
for i in "${!paths[@]}"; do
  [ "${versions[$i]}" = "$ref" ] \
    || fail "version drift: ${paths[$i]} is ${versions[$i]}, but ${ref_path} is ${ref} — all ${#paths[@]} manifests must agree"
done

# Dots are escaped so the lookup is literal: `0.1.0` must not be satisfied by
# a `## [0x1y0]` heading.
heading_re="^## \[${ref//./\\.}\]"
grep -qE "$heading_re" CHANGELOG.md \
  || fail "CHANGELOG.md has no '## [${ref}]' section, but the manifests carry ${ref} — rename '## [Unreleased]' in the bump PR"

if [ -z "$WANT" ]; then
  echo "release-preconditions: OK — ${#paths[@]} manifests agree on ${ref}, CHANGELOG.md has its section."
  exit 0
fi

[ "$ref" = "$WANT" ] \
  || fail "dispatched version is ${WANT}, but the manifests carry ${ref} — bump all ${#paths[@]} manifests (and the CHANGELOG) before dispatching"

# The remote is the authority on tags: the release checkout is shallow and
# tagless, so a local probe would always say "absent".
set +e
git ls-remote --exit-code --tags origin "refs/tags/v${WANT}" >/dev/null 2>&1
probe=$?
set -e
case "$probe" in
  0) fail "tag v${WANT} already exists on origin — bump the version or delete the tag first" ;;
  2) : ;; # --exit-code: the remote answered, no such tag
  *) fail "could not ask origin whether refs/tags/v${WANT} exists (git exited ${probe}) — refusing to guess" ;;
esac

echo "release-preconditions: OK — ${#paths[@]} manifests agree on ${WANT}, CHANGELOG.md has its section, v${WANT} is not on origin."
