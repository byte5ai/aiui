#!/usr/bin/env bash
# Self-test for `check-release-preconditions.sh` (#209).
#
# The guard is the only thing standing between a drifted tree and a signed,
# notarized, permanently-published release. A guard that silently stopped
# guarding would be worse than none, so it is exercised against fixture
# trees in CI on every PR — including the exact drift that shipped
# undetected for six minor versions (`companion/package.json` at 0.4.5
# against 0.10.1 everywhere else).
#
# Each fixture is a throwaway tree in a temp dir with the four manifests, a
# CHANGELOG and a copy of the guard under `scripts/`, so the guard resolves
# its repo root exactly the way it does in the real checkout. `git init`
# keeps git from walking up out of the fixture; the remote-tag cases point
# `origin` at a local repo with (or without) the tag.

set -uo pipefail

GUARD="$(cd "$(dirname "$0")" && pwd)/check-release-preconditions.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fails=0
rc=0
last_err=""

GIT_ID=(-c user.email=selftest@example.com -c user.name=selftest)

make_origin() { # dir [tag]
  git init -q "$1"
  git -C "$1" "${GIT_ID[@]}" commit -q --allow-empty -m init
  [ "$#" -lt 2 ] || git -C "$1" tag "$2"
}

make_tree() { # dir manifest_version changelog_version origin
  local d="$1" v="$2" cv="$3" origin="$4"
  mkdir -p "$d/scripts" "$d/companion/src-tauri" "$d/python"
  cp "$GUARD" "$d/scripts/"
  printf '[package]\nname = "aiui"\nversion = "%s"\n' "$v" > "$d/companion/src-tauri/Cargo.toml"
  printf '{"productName": "aiui", "version": "%s"}\n' "$v" > "$d/companion/src-tauri/tauri.conf.json"
  printf '[project]\nname = "aiui-mcp"\nversion = "%s"\n' "$v" > "$d/python/pyproject.toml"
  printf '{"name": "aiui-companion", "version": "%s"}\n' "$v" > "$d/companion/package.json"
  printf '# Changelog\n\n## [Unreleased]\n\n## [%s] — 2026-01-01\n\nstuff\n' "$cv" > "$d/CHANGELOG.md"
  git init -q "$d"
  git -C "$d" remote add origin "$origin"
}

run() { # dir [version]
  local dir="$1"
  shift
  last_err="$("$dir/scripts/check-release-preconditions.sh" "$@" 2>&1 >/dev/null)"
  rc=$?
}

check() { # name expected_exit dir [version]
  local name="$1" expect="$2"
  shift 2
  run "$@"
  if [ "$rc" = "$expect" ]; then
    echo "  ok      $name"
    return 0
  fi
  echo "  FAILED  $name (exit $rc, expected $expect)"
  echo "$last_err" | sed 's/^/            /'
  fails=$((fails + 1))
  return 1
}

expect_stderr() { # name needle...
  local name="$1"
  shift
  local needle
  for needle in "$@"; do
    case "$last_err" in
      *"$needle"*) ;;
      *)
        echo "  FAILED  $name — stderr never mentions '$needle'"
        echo "$last_err" | sed 's/^/            /'
        fails=$((fails + 1))
        return 1
        ;;
    esac
  done
  echo "  ok      $name (stderr names: $*)"
}

echo "check-release-preconditions self-test:"

# CI invokes the guard as `./scripts/check-release-preconditions.sh`, and so
# does every fixture below — a lost exec bit would turn the whole gate into
# a step that cannot run.
if [ -x "$GUARD" ]; then
  echo "  ok      the guard is executable"
else
  echo "  FAILED  $GUARD is not executable"
  fails=$((fails + 1))
fi

make_origin "$TMP/origin-clean"
make_origin "$TMP/origin-tagged" v9.9.9

# test_all_four_agree_passes
make_tree "$TMP/ok" 9.9.9 9.9.9 "$TMP/origin-clean"
check "all four manifests agree + CHANGELOG section" 0 "$TMP/ok"

# test_package_json_drift_fails — the real-world drift this guard was added
# for: the fourth manifest the old inline check never looked at.
make_tree "$TMP/pkgdrift" 9.9.9 9.9.9 "$TMP/origin-clean"
printf '{"name": "aiui-companion", "version": "0.4.5"}\n' > "$TMP/pkgdrift/companion/package.json"
check "companion/package.json drift is refused" 1 "$TMP/pkgdrift" \
  && expect_stderr "package.json drift message" "companion/package.json" "0.4.5" "9.9.9"

# Every manifest is in the same loop, so the first one drifting fails too.
make_tree "$TMP/pydrift" 9.9.9 9.9.9 "$TMP/origin-clean"
printf '[project]\nname = "aiui-mcp"\nversion = "9.9.8"\n' > "$TMP/pydrift/python/pyproject.toml"
check "python/pyproject.toml drift is refused" 1 "$TMP/pydrift" \
  && expect_stderr "pyproject drift message" "python/pyproject.toml" "9.9.8"

# test_missing_changelog_section_fails
make_tree "$TMP/nochangelog" 9.9.9 9.9.8 "$TMP/origin-clean"
check "a missing CHANGELOG section is refused" 1 "$TMP/nochangelog" \
  && expect_stderr "CHANGELOG message" "CHANGELOG.md" "9.9.9"

# test_dispatch_version_mismatch_fails
make_tree "$TMP/dispatch" 9.9.9 9.9.9 "$TMP/origin-clean"
check "a dispatched version the manifests do not carry is refused" 1 "$TMP/dispatch" 9.9.10 \
  && expect_stderr "dispatch mismatch message" "9.9.10" "9.9.9"

# Dispatch mode against a remote that does not have the tag.
check "a matching dispatched version passes" 0 "$TMP/dispatch" 9.9.9

# The probe must actually see REMOTE tags — this is the case the old
# `git rev-parse` guard could never catch on a shallow, tagless checkout.
make_tree "$TMP/tagged" 9.9.9 9.9.9 "$TMP/origin-tagged"
check "an already-released tag on origin is refused" 1 "$TMP/tagged" 9.9.9 \
  && expect_stderr "existing-tag message" "v9.9.9" "origin"

# test_tag_probe_uses_ls_remote — a textual regression guard. Reintroducing
# `git rev-parse` for the tag probe would reinstate a check that cannot fire
# on a shallow, tagless checkout, which is worse than none: it reads like a
# guard in review. Comments are stripped first, since the guard's own header
# names the call it replaced.
if grep -vE '^[[:space:]]*#' "$GUARD" | grep -qE 'git rev-parse'; then
  echo "  FAILED  the tag probe uses git rev-parse — invisible on a shallow checkout"
  fails=$((fails + 1))
elif ! grep -q 'git ls-remote --exit-code --tags origin' "$GUARD"; then
  echo "  FAILED  the tag probe no longer asks origin via git ls-remote"
  fails=$((fails + 1))
else
  echo "  ok      the tag probe asks origin, not the local (shallow) checkout"
fi

echo
if [ "$fails" -eq 0 ]; then
  echo "check-release-preconditions: self-test OK"
else
  echo "check-release-preconditions: $fails self-test failure(s)" >&2
  exit 1
fi
