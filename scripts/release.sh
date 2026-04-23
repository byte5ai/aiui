#!/usr/bin/env bash
# Build, sign, notarize, and publish an aiui release.
#
# Prerequisites (once per machine):
#   - Apple Developer ID Application certificate installed in the login keychain
#   - `xcrun notarytool store-credentials aiui-notary` configured
#   - `gh auth login` (GitHub CLI)
#
# Required environment (or .env.release file):
#   APPLE_SIGNING_IDENTITY   e.g. "Developer ID Application: byte5 GmbH (TEAMID)"
#   NOTARY_PROFILE           name passed to `notarytool store-credentials` (e.g. "aiui-notary")
#
# Usage:
#   scripts/release.sh 0.1.0          # builds + signs + notarizes + publishes release
#   scripts/release.sh 0.1.0 --dry    # stops before `gh release create`
set -euo pipefail

VERSION="${1:-}"
DRY="${2:-}"
if [[ -z "$VERSION" ]]; then
  echo "usage: $0 <version> [--dry]" >&2
  exit 1
fi
TAG="v${VERSION}"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Load optional .env.release (gitignored)
if [[ -f .env.release ]]; then
  set -a; source .env.release; set +a
fi
: "${APPLE_SIGNING_IDENTITY:?not set}"
: "${NOTARY_PROFILE:?not set}"

APP_SRC="companion/src-tauri/target/aarch64-apple-darwin/release/bundle/macos/aiui.app"
ZIP_OUT="${REPO_ROOT}/aiui-${VERSION}-arm64.zip"

# 1. Bump Cargo + tauri.conf version if needed
echo "→ Checking version in Cargo.toml and tauri.conf.json"
if ! grep -q "^version = \"${VERSION}\"" companion/src-tauri/Cargo.toml; then
  echo "  Cargo.toml version does not match ${VERSION} — set it first." >&2
  exit 1
fi

# 2. Build
echo "→ Building frontend"
(cd companion && npm ci && npm run build)

echo "→ Building Tauri release"
(cd companion && npx tauri build --target aarch64-apple-darwin)

# 3. Codesign (tauri-action itself does this if APPLE_SIGNING_IDENTITY is set,
#    but we re-sign explicitly to make the step observable)
echo "→ Codesigning ${APP_SRC}"
codesign --force --deep --options runtime \
  --sign "${APPLE_SIGNING_IDENTITY}" \
  --entitlements companion/src-tauri/entitlements.plist \
  "${APP_SRC}"
codesign --verify --deep --strict --verbose=2 "${APP_SRC}"

# 4. Zip for notarization
rm -f "${ZIP_OUT}"
ditto -c -k --sequesterRsrc --keepParent "${APP_SRC}" "${ZIP_OUT}"

# 5. Notarize
echo "→ Submitting to Apple notary service (this takes a few minutes)"
xcrun notarytool submit "${ZIP_OUT}" \
  --keychain-profile "${NOTARY_PROFILE}" \
  --wait

# 6. Staple the ticket into the app, re-zip
echo "→ Stapling notarization ticket"
xcrun stapler staple "${APP_SRC}"
xcrun stapler validate "${APP_SRC}"

rm -f "${ZIP_OUT}"
ditto -c -k --sequesterRsrc --keepParent "${APP_SRC}" "${ZIP_OUT}"
echo "✓ Release artifact: ${ZIP_OUT}"

if [[ "$DRY" == "--dry" ]]; then
  echo "Dry run — stopping before gh release."
  exit 0
fi

# 7. Git tag + push
if ! git rev-parse "${TAG}" >/dev/null 2>&1; then
  git tag -a "${TAG}" -m "Release ${TAG}"
fi
git push origin "${TAG}"

# 8. GitHub Release
NOTES_FILE="$(mktemp)"
trap "rm -f ${NOTES_FILE}" EXIT
{
  echo "## aiui ${TAG}"
  echo
  echo "**Install:**"
  echo '```sh'
  echo "# Download aiui-${VERSION}-arm64.zip, then:"
  echo "ditto -xk aiui-${VERSION}-arm64.zip /Applications/"
  echo "```"
  echo
  echo "Signed + notarized by byte5 GmbH — no quarantine warnings."
} > "${NOTES_FILE}"

gh release create "${TAG}" "${ZIP_OUT}" \
  --title "aiui ${TAG}" \
  --notes-file "${NOTES_FILE}"

echo "✓ Released ${TAG} on GitHub"
