#!/usr/bin/env bash
# Guard for the in-app updater feed (`latest.json`) — issue #190.
#
# `releases/latest/download/latest.json` is what every installed aiui polls.
# The invariant it must satisfy: **it never advertises a version without
# carrying an entry for every platform aiui ships for.** A feed missing a
# platform sends that platform's update check into the error path
# (`tauri-plugin-updater` raises `TargetsNotFound`), so those users see a
# failure rather than "up to date" — and never get the update.
#
# That is exactly what shipped in v0.10.1: `release-macos.yml` wrote a
# darwin-only feed and published it immediately, while the Windows entry was
# added later by a second, manually dispatched workflow. Between the two, every
# Windows client was broken; if the second dispatch was forgotten, permanently.
#
# The check also catches the *worse* failure mode — a feed whose platform entry
# points at an older release's artifact. That verifies and installs cleanly,
# leaving the client on the old version while the feed still offers the new
# one: a silent reinstall loop, and `/update` reports success with the feed's
# version rather than the installed one.
#
# Usage:
#   scripts/check-updater-feed.sh <latest.json> <expected-tag> <expected-version>
#
# Example:
#   scripts/check-updater-feed.sh latest.json v0.11.0 0.11.0
#
# Exit 0 = the feed is complete and self-consistent. Any other exit = do not
# publish; the message names the offending key.

set -euo pipefail

# Every platform aiui ships an updater artifact for. Adding `darwin-x86_64`
# later is a one-line change here — deliberately the single place this list
# lives, so a new platform cannot be half-added.
REQUIRED_PLATFORMS=(darwin-aarch64 windows-x86_64)

usage() {
  echo "usage: $0 <latest.json> <expected-tag> <expected-version>" >&2
  echo "   e.g. $0 latest.json v0.11.0 0.11.0" >&2
  exit 2
}

[ $# -eq 3 ] || usage
FEED="$1"
TAG="$2"
VERSION="$3"

fail() {
  echo "updater-feed: FAIL — $1" >&2
  exit 1
}

[ -f "$FEED" ] || fail "$FEED does not exist"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

FEED="$FEED" TAG="$TAG" VERSION="$VERSION" \
REQUIRED="${REQUIRED_PLATFORMS[*]}" python3 - <<'PY'
import json
import os
import sys

feed_path = os.environ["FEED"]
tag = os.environ["TAG"]
version = os.environ["VERSION"]
required = os.environ["REQUIRED"].split()

problems = []

try:
    with open(feed_path) as f:
        feed = json.load(f)
except json.JSONDecodeError as e:
    print(f"updater-feed: FAIL — {feed_path} is not valid JSON: {e}", file=sys.stderr)
    raise SystemExit(1)

if not isinstance(feed, dict):
    print(f"updater-feed: FAIL — {feed_path} is not a JSON object", file=sys.stderr)
    raise SystemExit(1)

# The version the feed advertises must be the one being released. A mismatch
# means the feed was assembled from the wrong build.
got_version = feed.get("version")
if got_version != version:
    problems.append(f'version is "{got_version}", expected "{version}"')

platforms = feed.get("platforms")
if not isinstance(platforms, dict):
    problems.append("no `platforms` object")
    platforms = {}

for name in required:
    entry = platforms.get(name)
    if entry is None:
        problems.append(
            f"missing platform `{name}` — every client on it would get "
            f"TargetsNotFound instead of an update"
        )
        continue
    if not isinstance(entry, dict):
        problems.append(f"`{name}` is not an object")
        continue

    sig = entry.get("signature")
    if not isinstance(sig, str) or not sig.strip():
        problems.append(f"`{name}` has an empty or missing signature")

    url = entry.get("url")
    if not isinstance(url, str) or not url.strip():
        problems.append(f"`{name}` has an empty or missing url")
        continue
    # The url must point into THIS release. This is what catches an entry
    # carried forward from a previous release: it would verify and install,
    # silently leaving the client on the old version.
    expected_prefix = f"/releases/download/{tag}/"
    if expected_prefix not in url:
        problems.append(
            f"`{name}` url does not point at {tag}: {url} — an entry from "
            f"another release installs the wrong artifact and verifies fine"
        )

# A platform we do not ship for is not fatal, but it is worth naming: it is
# either a leftover or a list that was not updated here.
for name in platforms:
    if name not in required:
        print(
            f"updater-feed: note — feed carries `{name}`, which is not in this "
            f"script's shipped-platform list",
            file=sys.stderr,
        )

if problems:
    print(f"updater-feed: FAIL — {feed_path} is not publishable:", file=sys.stderr)
    for p in problems:
        print(f"  - {p}", file=sys.stderr)
    raise SystemExit(1)

print(
    f"updater-feed: OK — {feed_path} advertises {version} with "
    f"{', '.join(required)}, all pointing at {tag}."
)
PY
