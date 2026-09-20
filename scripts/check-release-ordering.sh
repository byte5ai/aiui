#!/usr/bin/env bash
# Guard for the release step order — issue #191.
#
# The companion pins the bridge to its own version (`aiui-mcp==<version>`)
# in every registered remote's `~/.claude.json`. So the bridge has to exist
# on PyPI *before* the app that pins it can reach anyone: otherwise `uvx`
# on every remote host cannot resolve the pin and every aiui tool call
# returns nothing — for as long as the gap lasts, which is forever if the
# publish step fails and nobody re-runs it.
#
# The original ordering put PyPI last because PyPI versions are permanent.
# That weighs the wrong risk: a published bridge with no matching app is
# inert (nothing ever resolves it), while an app with no matching bridge is
# a dead feature on every remote the user owns.
#
# This is a textual guard on the workflow, not a test of the release — that
# one only runs for real on a release day. It exists so the ordering cannot
# be quietly reversed by a later edit.

set -euo pipefail

WF="${1:-.github/workflows/release-macos.yml}"

fail() {
  echo "release-ordering: FAIL — $1" >&2
  exit 1
}

[ -f "$WF" ] || fail "$WF does not exist"

line_of() { # step-name -> line number, or empty
  grep -n "^      - name: $1\$" "$WF" | head -1 | cut -d: -f1
}

PUBLISH="$(line_of 'Publish aiui-mcp to PyPI')"
WAIT="$(line_of 'Wait for aiui-mcp to be resolvable')"
RELEASE="$(line_of 'Tag + GitHub release')"

[ -n "$PUBLISH" ] || fail "no 'Publish aiui-mcp to PyPI' step found in $WF"
[ -n "$RELEASE" ] || fail "no 'Tag + GitHub release' step found in $WF"
[ -n "$WAIT" ] || fail "no 'Wait for aiui-mcp to be resolvable' step — upload
  returning 0 is not yet 'a remote can resolve it'; the index is not
  instantly consistent"

[ "$PUBLISH" -lt "$RELEASE" ] || fail "the PyPI publish (line $PUBLISH) runs AFTER
  the GitHub release (line $RELEASE). That ships a companion whose remote pin
  cannot resolve: every aiui tool call on every remote host fails until the
  bridge is published."

[ "$WAIT" -gt "$PUBLISH" ] && [ "$WAIT" -lt "$RELEASE" ] || fail "the resolve
  check (line $WAIT) must sit between the publish (line $PUBLISH) and the
  release (line $RELEASE)"

# The skip hole: a run with publish-pypi=false must not be able to produce a
# directly-promotable full release.
grep -q 'publish-pypi' "$WF" || fail "no publish-pypi input referenced at all"
if ! grep -q 'requires prerelease=true' "$WF"; then
  fail "no preflight guard rejecting publish-pypi=false with prerelease=false.
  Such a run cuts a full release with no bridge artifact, which pins every
  remote to a version that does not exist."
fi

# `--check-url` is what keeps a same-version recovery re-run working now
# that the publish comes first.
grep -q -- '--check-url' "$WF" || fail "uv publish has no --check-url, so a
  recovery re-run of the same version fails on already-uploaded files"

echo "release-ordering: OK — PyPI publish (line $PUBLISH) → resolve check (line $WAIT) → release (line $RELEASE)."
