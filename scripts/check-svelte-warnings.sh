#!/usr/bin/env bash
# check-svelte-warnings.sh — frontend type/a11y gate with a ratchet (#210).
#
# CI ran `svelte-check --threshold error`, which fails on errors and says
# nothing at all about warnings. There are 10 of them today (a11y, two
# files). With no gate the number can only drift upward, one PR at a time,
# and nobody sees it happen — the same shape of problem as a green Windows
# leg that never ran a test.
#
# So: errors fail the build, as before, and warnings fail it as soon as
# there are MORE than the recorded baseline. The count can fall freely.
#
# WHEN YOU FIX WARNINGS
# ---------------------
# Lower BASELINE to the new number in the same PR. The script tells you the
# number to write. Raising it is not a fix — a PR that needs a higher
# baseline is a PR adding warnings.
#
# Run locally:  scripts/check-svelte-warnings.sh
# CI:           .github/workflows/ci.yml -> job `companion`

set -euo pipefail

# Highest number of svelte-check warnings this repo tolerates. Measured on
# main: "0 errors and 10 warnings in 2 files".
BASELINE=10

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/companion"

OUT="$(npx svelte-check --tsconfig ./tsconfig.json --threshold warning --output human 2>&1 || true)"
echo "$OUT"

SUMMARY="$(printf '%s\n' "$OUT" | grep -E 'svelte-check found [0-9]+ errors? and [0-9]+ warnings?' | tail -1 || true)"

if [ -z "$SUMMARY" ]; then
  echo "svelte-warnings: FAIL — svelte-check printed no summary line; treat as a broken check, not a pass" >&2
  exit 1
fi

ERRORS="$(printf '%s\n' "$SUMMARY" | sed -E 's/.*found ([0-9]+) errors?.*/\1/')"
WARNINGS="$(printf '%s\n' "$SUMMARY" | sed -E 's/.*and ([0-9]+) warnings?.*/\1/')"

if [ "$ERRORS" -gt 0 ]; then
  echo "svelte-warnings: FAIL — $ERRORS error(s); errors are never tolerated" >&2
  exit 1
fi

if [ "$WARNINGS" -gt "$BASELINE" ]; then
  echo "svelte-warnings: FAIL — $WARNINGS warning(s), baseline is $BASELINE." >&2
  echo "  Fix the new ones. Do not raise BASELINE in $0 to make this pass." >&2
  exit 1
fi

if [ "$WARNINGS" -lt "$BASELINE" ]; then
  echo "svelte-warnings: OK — $WARNINGS warning(s), below the baseline of $BASELINE."
  echo "  Please lower BASELINE to $WARNINGS in $0 so the gain is held."
  exit 0
fi

echo "svelte-warnings: OK — 0 errors, $WARNINGS warning(s) at the baseline."
