#!/usr/bin/env bash
#
# check-doc-claims.sh — user-facing text must not promise automatic
# installs (#188, F-13).
#
# aiui DETECTS a new version and offers a one-click install; it never
# installs one on its own (SECURITY.md, README). The promise of
# self-installing updates survived the #188 fix in the one place most users
# read: the GitHub release page, generated from the notes template in
# release-macos.yml. That text shipped on every release, contradicting the
# security policy, with nothing to stop it.
#
# This guard scans every surface a user or security reporter reads for the
# phrasings that make that promise. Add a surface to FILES when one appears.
#
# Run locally:  scripts/check-doc-claims.sh
# CI:           .github/workflows/ci.yml -> job `skill-drift`
# Self-test:    scripts/check-doc-claims.sh --self-test

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Phrasings that promise an install without the user's click. Matched
# case-insensitively as extended regexes.
PATTERNS=(
  'update[s]? (themselves|itself)'
  'automatic(ally)? (install|apply|apply updates|update)'
  'install(s|ed)? (updates )?automatically'
  'self-(updat|install)'
  'in place via the in-app updater'
)

FILES=(
  README.md
  SECURITY.md
  CONTRIBUTING.md
  python/README.md
  .github/workflows/release-macos.yml
  .github/workflows/release-windows.yml
)

scan() { # file... -> prints offending lines, returns 1 on a hit
  local hit=0 f p
  for f in "$@"; do
    [ -f "$f" ] || continue
    for p in "${PATTERNS[@]}"; do
      if grep -nEi -- "$p" "$f" | sed "s|^|$f:|"; then
        hit=1
      fi
    done
  done
  return "$hit"
}

if [ "${1:-}" = "--self-test" ]; then
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  fails=0
  printf 'Signed + notarized. From v0.1.2 on, existing installations\nupdate themselves in place via the in-app updater.\n' > "$TMP/bad.md"
  printf 'aiui checks for new versions and offers a one-click install.\n' > "$TMP/good.md"
  if scan "$TMP/bad.md" >/dev/null; then
    echo "  FAILED  the v0.11.0 release-notes wording was not flagged"; fails=$((fails + 1))
  else
    echo "  ok      the v0.11.0 release-notes wording is flagged"
  fi
  if scan "$TMP/good.md" >/dev/null; then
    echo "  ok      the one-click wording passes"
  else
    echo "  FAILED  the one-click wording was flagged"; fails=$((fails + 1))
  fi
  [ "$fails" -eq 0 ] && echo "check-doc-claims: self-test OK" || { echo "check-doc-claims: $fails self-test failure(s)" >&2; exit 1; }
  exit 0
fi

cd "$ROOT"
if ! scan "${FILES[@]}"; then
  echo "check-doc-claims: FAIL — the lines above promise an automatic install." >&2
  echo "aiui detects updates and offers a one-click install; say that instead (#188)." >&2
  exit 1
fi
echo "check-doc-claims: OK — no surface promises an automatic install (${#FILES[@]} files)."
