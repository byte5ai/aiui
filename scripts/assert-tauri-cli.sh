#!/usr/bin/env bash
# Assert `npx tauri` is the CLI package-lock.json locks — #192.
#
# The Windows jobs used to run `npm ci` and then bundle with a *globally*
# installed `@tauri-apps/cli@^2`, resolved at build time and discarded.
# So the bundler that produced the shipped installer — and the `.sig` the
# updater verifies before installing it — was whichever version npm
# happened to resolve that minute, and it was recorded nowhere. "Which
# bundler built the .exe user X is running?" had no answer. macOS always
# did this correctly with `npx tauri build`; this is the check that keeps
# both platforms on the locked CLI.
#
# Run with `working-directory: companion` (where package-lock.json and
# node_modules/.bin/tauri live).
#
# Usage: assert-tauri-cli.sh [path-to-package-lock.json]

set -euo pipefail

LOCK="${1:-package-lock.json}"

[ -f "$LOCK" ] || { echo "tauri-cli: FAIL — $LOCK does not exist" >&2; exit 1; }

# The path travels in the environment, not in argv: `node -e` shifts argv
# and the lock path is a Windows path under Git Bash.
WANT="$(AIUI_LOCK="$LOCK" node -e '
  const lock = require("path").resolve(process.env.AIUI_LOCK);
  const pkg = require(lock).packages["node_modules/@tauri-apps/cli"];
  if (!pkg || !pkg.version) { console.error("no @tauri-apps/cli in " + lock); process.exit(1); }
  process.stdout.write(pkg.version);
')"

[ -n "$WANT" ] || { echo "tauri-cli: FAIL — could not read the locked @tauri-apps/cli version" >&2; exit 1; }

# `npx tauri` resolves this; if it is missing npx would silently fetch some
# other version from the registry, which is the very thing being ruled out.
[ -e node_modules/.bin/tauri ] || {
  echo "tauri-cli: FAIL — node_modules/.bin/tauri is missing; \`npm ci\` has not run in $PWD" >&2
  exit 1
}

# `tauri --version` has printed both "tauri-cli 2.10.1" and a bare version
# across 2.x, and npm can prepend notices — take the first semver it emits.
# `|| true` because `head` closing the pipe early is a SIGPIPE, and under
# `pipefail` that would abort the script instead of reaching the check below.
GOT="$(npx tauri --version 2>/dev/null | tr -d '\r' | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"

[ -n "$GOT" ] || { echo "tauri-cli: FAIL — \`npx tauri --version\` printed no version" >&2; exit 1; }

if [ "$GOT" != "$WANT" ]; then
  echo "tauri-cli: FAIL — \`npx tauri\` is ${GOT}, but ${LOCK} locks ${WANT}." >&2
  echo "  A bundler outside the lockfile is a second source of truth, and the" >&2
  echo "  one that ships is the one NOT under review. Drop any global" >&2
  echo "  \`npm install --global @tauri-apps/cli\` and build with \`npx tauri\`." >&2
  exit 1
fi

echo "tauri-cli: OK — npx tauri ${GOT} matches the version locked in ${LOCK}."
