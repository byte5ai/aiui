#!/usr/bin/env bash
#
# check-leaks.sh — this repo is PUBLIC; keep private infrastructure out of it
# (review F-16).
#
# The fix series for 0.11.0 added ~20 test fixtures naming a real internal
# host and user@host pairs, next to older sites naming private repos and a
# home-directory path. Nothing checked for any of it.
#
# Two pattern sources:
#   - built-in, generic patterns that are never legitimate here (a private
#     credential-store layout);
#   - AIUI_LEAK_PATTERNS, newline-separated extended regexes from the
#     repository variable of the same name. The concrete hostnames and
#     account names live THERE, not in this file — a guard that spelled them
#     out would be a leak itself.
#
# Run locally:  AIUI_LEAK_PATTERNS=$'name1\nname2' scripts/check-leaks.sh
# CI:           .github/workflows/ci.yml -> job `skill-drift`

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

patterns=('\.github_tokens/')
if [ -n "${AIUI_LEAK_PATTERNS:-}" ]; then
  while IFS= read -r p; do
    [ -n "$p" ] && patterns+=("$p")
  done <<< "$AIUI_LEAK_PATTERNS"
else
  echo "check-leaks: AIUI_LEAK_PATTERNS is not set — checking the built-in patterns only."
fi

hits=0
for p in "${patterns[@]}"; do
  # Lockfiles carry third-party content; this script names its own patterns.
  if git grep -n -I -i -E -e "$p" -- . \
       ':!*.lock' ':!companion/package-lock.json' ':!scripts/check-leaks.sh' >/tmp/aiui-leaks.$$ 2>/dev/null; then
    hits=$((hits + $(wc -l < /tmp/aiui-leaks.$$)))
    # Print file:line only — echoing the matched text would put the pattern
    # into a public CI log.
    cut -d: -f1,2 /tmp/aiui-leaks.$$ | sed 's/^/  leak: /'
  fi
done
rm -f /tmp/aiui-leaks.$$

if [ "$hits" -gt 0 ]; then
  echo "check-leaks: FAIL — $hits line(s) name private infrastructure (see above)." >&2
  echo "Use neutral placeholders: example-host, dev@example-host, ~/.config/demo/token." >&2
  exit 1
fi
echo "check-leaks: OK — ${#patterns[@]} pattern(s), no matches."
