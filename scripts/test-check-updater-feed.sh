#!/usr/bin/env bash
# Self-test for `check-updater-feed.sh` (#190).
#
# The guard is what stops a half-published updater feed from reaching users.
# A guard that silently stopped guarding would be worse than none, so it is
# exercised in CI against fixtures — including the exact shape that shipped
# in v0.10.1.

set -uo pipefail

GUARD="$(dirname "$0")/check-updater-feed.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fails=0

check() { # name expected_exit args...
  local name="$1" expect="$2"
  shift 2
  local out rc
  out="$("$GUARD" "$@" 2>&1)"
  rc=$?
  if [ "$rc" = "$expect" ]; then
    echo "  ok      $name"
  else
    echo "  FAILED  $name (exit $rc, expected $expect)"
    echo "$out" | sed 's/^/            /'
    fails=$((fails + 1))
  fi
}

mutate() { # source dest python-expression-on-d
  python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
$3
json.dump(d, open(sys.argv[2], 'w'))
" "$1" "$2"
}

cat > "$TMP/good.json" <<'JSON'
{
  "version": "0.11.0",
  "platforms": {
    "darwin-aarch64": {
      "signature": "dW50cnVzdGVk...",
      "url": "https://github.com/byte5ai/aiui/releases/download/v0.11.0/aiui-0.11.0-updater-arm64.tar.gz"
    },
    "windows-x86_64": {
      "signature": "dW50cnVzdGVk...",
      "url": "https://github.com/byte5ai/aiui/releases/download/v0.11.0/aiui_0.11.0_x64-setup.exe"
    }
  }
}
JSON

echo "check-updater-feed self-test:"
check "a complete feed passes" 0 "$TMP/good.json" v0.11.0 0.11.0

# The v0.10.1 bug itself: macOS published, Windows entry not yet added.
mutate "$TMP/good.json" "$TMP/nowin.json" "del d['platforms']['windows-x86_64']"
check "missing windows-x86_64 is refused" 1 "$TMP/nowin.json" v0.11.0 0.11.0

mutate "$TMP/good.json" "$TMP/nomac.json" "del d['platforms']['darwin-aarch64']"
check "missing darwin-aarch64 is refused" 1 "$TMP/nomac.json" v0.11.0 0.11.0

# The worse failure mode: an entry carried forward from an older release.
# It verifies and installs, leaving the client on the old version forever.
mutate "$TMP/good.json" "$TMP/carried.json" \
  "d['platforms']['windows-x86_64']['url'] = 'https://github.com/byte5ai/aiui/releases/download/v0.10.1/aiui_0.10.1_x64-setup.exe'"
check "an entry pointing at another release is refused" 1 "$TMP/carried.json" v0.11.0 0.11.0

mutate "$TMP/good.json" "$TMP/emptysig.json" "d['platforms']['darwin-aarch64']['signature'] = ''"
check "an empty signature is refused" 1 "$TMP/emptysig.json" v0.11.0 0.11.0

mutate "$TMP/good.json" "$TMP/nourl.json" "del d['platforms']['windows-x86_64']['url']"
check "a missing url is refused" 1 "$TMP/nourl.json" v0.11.0 0.11.0

check "a version mismatch is refused" 1 "$TMP/good.json" v0.11.0 0.12.0
check "a tag mismatch is refused" 1 "$TMP/good.json" v0.12.0 0.11.0

echo '{"version": "0.11.0"}' > "$TMP/noplat.json"
check "a feed with no platforms is refused" 1 "$TMP/noplat.json" v0.11.0 0.11.0

echo 'not json at all' > "$TMP/bad.json"
check "unparsable JSON is refused" 1 "$TMP/bad.json" v0.11.0 0.11.0

check "a missing file is refused" 1 "$TMP/nope.json" v0.11.0 0.11.0
check "wrong argument count exits 2" 2 "$TMP/good.json" v0.11.0

# An unknown platform is worth a note, not a failure — it is either a
# leftover or a list this script has not caught up with.
mutate "$TMP/good.json" "$TMP/extra.json" \
  "d['platforms']['linux-x86_64'] = {'signature': 's', 'url': 'https://github.com/byte5ai/aiui/releases/download/v0.11.0/x'}"
check "an unknown platform is a note, not a failure" 0 "$TMP/extra.json" v0.11.0 0.11.0

echo
if [ "$fails" -eq 0 ]; then
  echo "check-updater-feed: self-test OK"
else
  echo "check-updater-feed: $fails self-test failure(s)" >&2
  exit 1
fi
