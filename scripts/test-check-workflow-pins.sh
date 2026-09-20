#!/usr/bin/env bash
# Self-test for `check-workflow-pins.sh` (#192).
#
# The guard is the only thing standing between a future edit and a
# floating action ref in a job that holds the Developer ID certificate,
# the notary key, the minisign updater key and the PyPI token. A guard
# that silently stopped guarding would be worse than none, so it is
# exercised against fixtures on every PR — including the exact shapes
# that were in the tree before #192: `@v4`, `@stable`, `version: latest`
# and a global `@tauri-apps/cli` install.
#
# The last case runs the guard against the repo's real workflows, so a
# reintroduced floating ref fails here as well as in CI.

set -uo pipefail

GUARD="$(dirname "$0")/check-workflow-pins.sh"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fails=0

SHA=0123456789abcdef0123456789abcdef01234567

check() { # name expected_exit dir
  local name="$1" expect="$2" dir="$3"
  local out rc
  out="$("$GUARD" "$dir" 2>&1)"
  rc=$?
  if [ "$rc" = "$expect" ]; then
    echo "  ok      $name"
  else
    echo "  FAILED  $name (exit $rc, expected $expect)"
    echo "$out" | sed 's/^/            /'
    fails=$((fails + 1))
  fi
}

fixture() { # name -> prints the dir it created
  local dir="$TMP/$1"
  mkdir -p "$dir"
  cat > "$dir/w.yml"
  echo "$dir"
}

echo "check-workflow-pins self-test:"

GOOD="$(fixture good <<YAML
jobs:
  build:
    steps:
      - uses: actions/checkout@${SHA} # v4.2.2
      - uses: dtolnay/rust-toolchain@${SHA} # master @ 2026-09-11
        with:
          toolchain: 1.98.1
      - uses: astral-sh/setup-uv@${SHA} # v3.2.4
        with:
          version: "0.12.17"
      - name: Tauri build
        run: npx tauri build --bundles nsis
YAML
)"
check "a fully pinned workflow passes" 0 "$GOOD"

# The pre-#192 tree, one failure mode at a time.
MOVING="$(fixture moving <<YAML
jobs:
  build:
    steps:
      - uses: actions/checkout@v4
YAML
)"
check "a moving major tag (@v4) is refused" 1 "$MOVING"

BRANCH="$(fixture branch <<YAML
jobs:
  build:
    steps:
      - uses: dtolnay/rust-toolchain@stable
YAML
)"
check "a branch ref (@stable) is refused" 1 "$BRANCH"

SHORT="$(fixture short <<YAML
jobs:
  build:
    steps:
      - uses: actions/checkout@11bd719 # v4.2.2
YAML
)"
check "an abbreviated SHA is refused" 1 "$SHORT"

# A bare SHA is safe but unreviewable: nobody can tell which version it is.
NOCOMMENT="$(fixture nocomment <<YAML
jobs:
  build:
    steps:
      - uses: actions/checkout@${SHA}
YAML
)"
check "a SHA with no version comment is refused" 1 "$NOCOMMENT"

UVLATEST="$(fixture uvlatest <<YAML
jobs:
  build:
    steps:
      - uses: astral-sh/setup-uv@${SHA} # v3.2.4
        with:
          version: latest
YAML
)"
check "setup-uv with version: latest is refused" 1 "$UVLATEST"

GLOBALCLI="$(fixture globalcli <<YAML
jobs:
  build:
    steps:
      - uses: actions/checkout@${SHA} # v4.2.2
      - name: Install Tauri CLI
        run: npm install --global @tauri-apps/cli@^2
YAML
)"
check "a global @tauri-apps/cli install is refused" 1 "$GLOBALCLI"

# `uses:` can also appear on its own line under a `- name:` step.
NAMEDSTEP="$(fixture namedstep <<YAML
jobs:
  build:
    steps:
      - name: Checkout the tagged commit
        uses: actions/checkout@v4
        with:
          ref: v1.0.0
YAML
)"
check "an unpinned \`uses:\` under a named step is refused" 1 "$NAMEDSTEP"

# A local composite action has no upstream that can move under us.
LOCAL="$(fixture local <<YAML
jobs:
  build:
    steps:
      - uses: ./.github/actions/setup
YAML
)"
check "a local action reference is allowed" 0 "$LOCAL"

mkdir -p "$TMP/empty"
check "an empty workflow dir is refused" 1 "$TMP/empty"
check "a missing dir is refused" 1 "$TMP/nope"

check "the repo's own workflows are pinned" 0 "$REPO_ROOT/.github/workflows"

echo
if [ "$fails" -eq 0 ]; then
  echo "check-workflow-pins: self-test OK"
else
  echo "check-workflow-pins: $fails self-test failure(s)" >&2
  exit 1
fi
