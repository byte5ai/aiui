#!/usr/bin/env bash
# Record the Xcode that builds on this runner and refuse any other major — #291.
#
# The macOS jobs run on macrunner, whose image carries a single Xcode. Which
# one links the binary decides the SDK the app is built against — and an
# AppKit app picks up new system behaviour by the SDK it was linked with —
# so it belongs in the record next to the Rust compiler
# (assert-rust-toolchain.sh). An image update that moves Xcode fails here
# instead of silently changing what ships; raise EXPECTED_MAJOR in the change
# that verifies the new toolchain.

set -euo pipefail

EXPECTED_MAJOR="27"

# The image documents its tool versions here; into the log with them.
if [ -r /Users/Shared/macrunner-image.txt ]; then
  cat /Users/Shared/macrunner-image.txt
fi

xcodebuild -version
echo "macOS SDK: $(xcrun --sdk macosx --show-sdk-version)"

GOT="$(xcodebuild -version | awk '/^Xcode /{split($2, v, "."); print v[1]; exit}')"
if [ "$GOT" != "$EXPECTED_MAJOR" ]; then
  echo "assert-xcode: FAIL — this runner has Xcode ${GOT:-<none>}, but the build is pinned to Xcode ${EXPECTED_MAJOR}." >&2
  echo "  Verify the new toolchain (arm64-only bundle, minimum macOS from tauri.conf.json)" >&2
  echo "  and raise EXPECTED_MAJOR in the same change." >&2
  exit 1
fi

echo "assert-xcode: OK — Xcode ${GOT}."
