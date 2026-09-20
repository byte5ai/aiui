#!/usr/bin/env bash
# Guard for the release supply chain — issue #192.
#
# The macOS and Windows release jobs hold, in one environment, the
# Developer ID certificate, the App Store Connect notary key, the
# minisign updater private key and the PyPI publish token — and `aiui`
# installs its own updates, so anything signed with that minisign key
# reaches every installed companion with no user-visible install step.
# A third-party action resolved from a mutable ref (`@v4`, `@v2`, and
# worst of all `@stable`, which is a *branch*) is therefore a direct path
# from "an upstream repo changed hands" to "signed code on every user's
# machine".
#
# So: every `uses:` is pinned to a full 40-char commit SHA, and this
# script is what stops the next edit from quietly reintroducing a
# floating ref. Same for the two other unpinned inputs that decide what
# actually ships — the uv binary downloaded at release time, and a
# globally installed Tauri CLI that bypasses the version
# `companion/package-lock.json` locks.
#
# It is a textual guard on the workflows, not a test of a release; a real
# release only runs on a release day.
#
# Usage: check-workflow-pins.sh [workflow-dir]   (default .github/workflows)

set -uo pipefail

DIR="${1:-.github/workflows}"

fails=0

fail() { # file line message
  echo "workflow-pins: FAIL — $1:$2 $3" >&2
  fails=$((fails + 1))
}

if [ ! -d "$DIR" ]; then
  echo "workflow-pins: FAIL — $DIR is not a directory" >&2
  exit 1
fi

shopt -s nullglob
files=("$DIR"/*.yml "$DIR"/*.yaml)
shopt -u nullglob

if [ "${#files[@]}" -eq 0 ]; then
  echo "workflow-pins: FAIL — no workflow files in $DIR" >&2
  exit 1
fi

checked=0

for f in "${files[@]}"; do
  # --- 1. every `uses:` is a 40-char SHA with a readable version comment
  while IFS=: read -r lineno line; do
    [ -n "$lineno" ] || continue
    ref="${line#*uses:}"
    ref="${ref#"${ref%%[![:space:]]*}"}"   # ltrim
    checked=$((checked + 1))

    # A local composite action has no upstream to move under us.
    case "$ref" in ./*) continue ;; esac

    if ! printf '%s' "$ref" | grep -Eq '@[0-9a-f]{40}([[:space:]]|$)'; then
      fail "$f" "$lineno" "\`$ref\` is not pinned to a 40-char commit SHA.
            A tag or branch ref is resolved at dispatch time, so an upstream
            retag reaches a job that holds the signing and publishing keys.
            Resolve it with: gh api repos/<owner>/<repo>/commits/<tag> --jq .sha"
      continue
    fi

    if ! printf '%s' "$ref" | grep -Eq '@[0-9a-f]{40}[[:space:]]+#[[:space:]]*[^[:space:]]'; then
      fail "$f" "$lineno" "\`$ref\` is pinned but carries no trailing
            \`# vX.Y.Z\` comment. A bare SHA is unreadable and unbumpable —
            nobody can tell at a glance which version they are reviewing."
    fi
  done < <(grep -nE '^[[:space:]]*(-[[:space:]]*)?uses:' "$f")

  # --- 2. no floating uv binary
  while IFS=: read -r lineno _; do
    [ -n "$lineno" ] || continue
    fail "$f" "$lineno" "\`version: latest\` downloads whatever uv shipped that
            morning into a job holding UV_PUBLISH_TOKEN. Pin an exact uv
            version next to the setup-uv SHA."
  done < <(grep -nE '^[[:space:]]*version:[[:space:]]*"?latest"?[[:space:]]*$' "$f")

  # --- 3. no globally installed Tauri CLI
  while IFS=: read -r lineno _; do
    [ -n "$lineno" ] || continue
    fail "$f" "$lineno" "a global \`@tauri-apps/cli\` install bypasses the
            version \`companion/package-lock.json\` locks, so the bundler that
            produces the shipped installer and the \`.sig\` the updater trusts
            is whatever npm resolved that minute — and it is never recorded.
            Use \`npx tauri …\` after \`npm ci\`, as the macOS job does."
  done < <(grep -n 'npm install --global @tauri-apps/cli' "$f")
done

if [ "$fails" -eq 0 ]; then
  echo "workflow-pins: OK — $checked \`uses:\` refs in ${#files[@]} workflow(s) pinned by SHA; no floating uv or Tauri CLI."
  exit 0
fi

echo "workflow-pins: $fails problem(s) in $DIR" >&2
exit 1
