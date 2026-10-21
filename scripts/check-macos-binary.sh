#!/usr/bin/env bash
# The macOS bundle is arm64 and runs from the macOS tauri.conf.json promises — #291.
#
# The build moved to Xcode 27 on macrunner, and Apple lists macOS 12 as the
# lowest target Xcode 27 supports, while aiui declares 11.0. A toolchain that
# raised the binary's minimum would cut those users off with nothing failing
# — the app would simply refuse to launch on their machine after an update.
# So the shipped Mach-O is read back: its architectures, the `minos` its
# LC_BUILD_VERSION records, and the Info.plist LSMinimumSystemVersion must
# all match what tauri.conf.json declares.
#
# Usage: check-macos-binary.sh <path/to/aiui.app> [path/to/tauri.conf.json]

set -euo pipefail

APP="${1:?usage: check-macos-binary.sh <path/to/aiui.app> [tauri.conf.json]}"
CONF="${2:-companion/src-tauri/tauri.conf.json}"
BIN="$APP/Contents/MacOS/aiui"

fail() { echo "macos-binary: FAIL — $*" >&2; exit 1; }

[ -f "$BIN" ] || fail "$BIN does not exist"

ARCHS="$(lipo -archs "$BIN")"
[ "$ARCHS" = "arm64" ] || fail "$BIN is built for '${ARCHS}', expected 'arm64' only"

WANT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["bundle"]["macOS"]["minimumSystemVersion"])' "$CONF")"
[ -n "$WANT" ] || fail "no bundle.macOS.minimumSystemVersion in $CONF"

BUILD="$(vtool -show-build "$BIN")"
echo "$BUILD"
MINOS="$(printf '%s\n' "$BUILD" | awk '$1 == "minos" {print $2; exit}')"
SDK="$(printf '%s\n' "$BUILD" | awk '$1 == "sdk" {print $2; exit}')"
[ "$MINOS" = "$WANT" ] || fail "$BIN records minos ${MINOS:-<none>}, but $CONF declares ${WANT}"

PLIST_MIN="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$APP/Contents/Info.plist")"
[ "$PLIST_MIN" = "$WANT" ] || fail "Info.plist LSMinimumSystemVersion is ${PLIST_MIN}, expected ${WANT}"

echo "macos-binary: OK — ${ARCHS}, minos ${MINOS} (SDK ${SDK}), LSMinimumSystemVersion ${PLIST_MIN}."
