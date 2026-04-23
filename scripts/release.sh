#!/usr/bin/env bash
# Build, sign, notarize, and publish an aiui release, including the updater
# feed (latest.json) used by tauri-plugin-updater on running clients.
#
# Prerequisites (once per machine):
#   - Apple Developer ID Application certificate in the build keychain
#   - `xcrun notarytool store-credentials` registered in the build keychain
#   - Updater signing keypair generated via `tauri signer generate`
#   - `gh auth login` (GitHub CLI)
#
# Required environment (loaded from .env.release):
#   APPLE_SIGNING_IDENTITY
#   NOTARY_PROFILE
#   BUILD_KEYCHAIN                   absolute path to keychain-db
#   BUILD_KEYCHAIN_PASS_FILE         path to file holding keychain password
#   TAURI_SIGNING_PRIVATE_KEY_PATH   minisign private key for updater feed
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  (optional; empty if no password)
#
# Usage:
#   scripts/release.sh 0.1.2
#   scripts/release.sh 0.1.2 --dry
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

if [[ -f .env.release ]]; then
  set -a; source .env.release; set +a
fi
: "${APPLE_SIGNING_IDENTITY:?not set}"
: "${NOTARY_PROFILE:?not set}"
: "${BUILD_KEYCHAIN:?not set}"
: "${BUILD_KEYCHAIN_PASS_FILE:?not set}"
: "${TAURI_SIGNING_PRIVATE_KEY_PATH:?not set}"
# Tauri bundler reads TAURI_SIGNING_PRIVATE_KEY (literal key content) during
# `tauri build`, not the _PATH variant. Load the file content here.
export TAURI_SIGNING_PRIVATE_KEY="$(cat "${TAURI_SIGNING_PRIVATE_KEY_PATH}")"
export TAURI_SIGNING_PRIVATE_KEY_PATH
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

KC_PASS="$(cat "$BUILD_KEYCHAIN_PASS_FILE")"
echo "→ Unlocking $BUILD_KEYCHAIN"
security unlock-keychain -p "$KC_PASS" "$BUILD_KEYCHAIN"
security list-keychains -d user -s "$BUILD_KEYCHAIN" $(security list-keychains -d user | tr -d '"' | grep -v "$BUILD_KEYCHAIN")

APP_DIR="companion/src-tauri/target/aarch64-apple-darwin/release/bundle/macos"
APP_SRC="${APP_DIR}/aiui.app"
DIRECT_ZIP="${REPO_ROOT}/aiui-${VERSION}-arm64.zip"
DIRECT_DMG="${REPO_ROOT}/aiui-${VERSION}-arm64.dmg"
UPDATER_BUNDLE="${APP_DIR}/aiui.app.tar.gz"
UPDATER_SIG="${UPDATER_BUNDLE}.sig"

echo "→ Checking Cargo.toml version"
if ! grep -q "^version = \"${VERSION}\"" companion/src-tauri/Cargo.toml; then
  echo "  Cargo.toml version does not match ${VERSION} — bump it first." >&2
  exit 1
fi

echo "→ Building frontend"
(cd companion && npm ci && npm run build)

echo "→ Building Tauri release (signs .app + emits .app.tar.gz + .sig for updater)"
(cd companion && npx tauri build --target aarch64-apple-darwin)

echo "→ Codesigning ${APP_SRC}"
codesign --force --deep --options runtime \
  --sign "${APPLE_SIGNING_IDENTITY}" \
  --entitlements companion/src-tauri/entitlements.plist \
  "${APP_SRC}"
codesign --verify --deep --strict --verbose=2 "${APP_SRC}"

rm -f "${DIRECT_ZIP}"
ditto -c -k --sequesterRsrc --keepParent "${APP_SRC}" "${DIRECT_ZIP}"

echo "→ Submitting ${DIRECT_ZIP} to Apple notary service"
xcrun notarytool submit "${DIRECT_ZIP}" \
  --keychain-profile "${NOTARY_PROFILE}" \
  --keychain "${BUILD_KEYCHAIN}" \
  --wait

echo "→ Stapling notarization ticket"
xcrun stapler staple "${APP_SRC}"
xcrun stapler validate "${APP_SRC}"

# Re-create the distributable zip AFTER stapling so the ticket is included.
rm -f "${DIRECT_ZIP}"
ditto -c -k --sequesterRsrc --keepParent "${APP_SRC}" "${DIRECT_ZIP}"

echo "→ Building DMG (appdmg, branded background + drag-to-Applications)"
rm -f "${DIRECT_DMG}"
(cd companion && npx appdmg src-tauri/dmg/config.json "${DIRECT_DMG}")
codesign --force --sign "${APPLE_SIGNING_IDENTITY}" "${DIRECT_DMG}"
xcrun notarytool submit "${DIRECT_DMG}" \
  --keychain-profile "${NOTARY_PROFILE}" \
  --keychain "${BUILD_KEYCHAIN}" \
  --wait
xcrun stapler staple "${DIRECT_DMG}"

# Re-create the updater bundle after stapling too, then re-sign it. The
# tauri signer picks the key path up from TAURI_SIGNING_PRIVATE_KEY_PATH,
# no CLI flags needed (and mixing them with env vars errors out).
rm -f "${UPDATER_BUNDLE}" "${UPDATER_SIG}"
tar -C "${APP_DIR}" -czf "${UPDATER_BUNDLE}" aiui.app
UPDATER_BUNDLE_ABS="${REPO_ROOT}/${UPDATER_BUNDLE}"
# Tauri signer CLI errors out if both TAURI_SIGNING_PRIVATE_KEY and
# TAURI_SIGNING_PRIVATE_KEY_PATH are set — unset PATH for just this call.
(cd companion && env -u TAURI_SIGNING_PRIVATE_KEY_PATH \
  npx tauri signer sign "${UPDATER_BUNDLE_ABS}") >/dev/null
SIG_FILE_CONTENT="$(cat "${UPDATER_SIG}")"
# tauri-updater expects the literal sig-file content as the signature field.
SIG_JSON=$(printf '%s' "${SIG_FILE_CONTENT}" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')
echo "✓ updater signature written to ${UPDATER_SIG}"

# Build latest.json describing this release.
PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
LATEST_JSON="${REPO_ROOT}/latest.json"
cat > "${LATEST_JSON}" <<JSON
{
  "version": "${VERSION}",
  "notes": "aiui ${TAG} — see https://github.com/byte5ai/aiui/releases/tag/${TAG}",
  "pub_date": "${PUB_DATE}",
  "platforms": {
    "darwin-aarch64": {
      "signature": ${SIG_JSON},
      "url": "https://github.com/byte5ai/aiui/releases/download/${TAG}/aiui-${VERSION}-updater-arm64.tar.gz"
    }
  }
}
JSON
echo "✓ Wrote ${LATEST_JSON}"

UPDATER_NAMED="${REPO_ROOT}/aiui-${VERSION}-updater-arm64.tar.gz"
cp "${UPDATER_BUNDLE}" "${UPDATER_NAMED}"

if [[ "$DRY" == "--dry" ]]; then
  echo "Dry run — artifacts:"
  echo "  ${DIRECT_DMG}"
  echo "  ${DIRECT_ZIP}"
  echo "  ${UPDATER_NAMED}"
  echo "  ${LATEST_JSON}"
  exit 0
fi

if ! git rev-parse "${TAG}" >/dev/null 2>&1; then
  git tag -a "${TAG}" -m "Release ${TAG}"
fi
git push origin "${TAG}"

NOTES_FILE="$(mktemp)"
trap "rm -f ${NOTES_FILE}" EXIT
cat > "${NOTES_FILE}" <<NOTES_EOF
## aiui ${TAG}

Signed + notarized by high5 ventures GmbH. From v0.1.2 on, existing installations
update themselves in place via the in-app updater.

**Fresh install:** Download \`aiui-${VERSION}-arm64.dmg\`, double-click, drag aiui.app into Applications, launch once.

(Zip also provided for scripted installs: \`ditto -xk aiui-${VERSION}-arm64.zip /Applications/\`.)

See the [full diff](https://github.com/byte5ai/aiui/commits/${TAG}).
NOTES_EOF

gh release create "${TAG}" \
  "${DIRECT_DMG}" \
  "${DIRECT_ZIP}" \
  "${UPDATER_NAMED}" \
  "${LATEST_JSON}" \
  --repo byte5ai/aiui \
  --title "aiui ${TAG}" \
  --notes-file "${NOTES_FILE}"

echo "✓ Released ${TAG} on GitHub"
