#!/usr/bin/env bash
# Assert the compiler in this job is the one `rust-toolchain.toml` pins — #192.
#
# Dropping in a `rust-toolchain.toml` and assuming it wins is the trap this
# exists for. In rustup's resolution order a `RUSTUP_TOOLCHAIN` in the
# environment outranks a directory override, and `dtolnay/rust-toolchain`
# both defaults a toolchain and runs `rustup default` with
# `continue-on-error: true` — so the file can be silently ignored in CI
# while working perfectly on a laptop. That is the worst of both worlds:
# the release claims a pinned compiler and does not have one.
#
# Run from the directory cargo actually builds in, so what is measured is
# what compiles: `working-directory: companion/src-tauri`.
#
# Usage: assert-rust-toolchain.sh [path-to-rust-toolchain.toml]
#        (default: ../../rust-toolchain.toml — relative on purpose;
#        $GITHUB_WORKSPACE is a backslashed Windows path under Git Bash)

set -euo pipefail

TOML="${1:-../../rust-toolchain.toml}"

[ -f "$TOML" ] || { echo "rust-toolchain: FAIL — $TOML does not exist" >&2; exit 1; }

# `|| true`: `head` closing the pipe is a SIGPIPE, which `pipefail` would
# turn into an abort before the empty check below can report anything.
WANT="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$TOML" | head -1 || true)"
[ -n "$WANT" ] || { echo "rust-toolchain: FAIL — no channel in $TOML" >&2; exit 1; }

GOT="$(rustc --version | tr -d '\r' | awk '{print $2}')"

if [ "$GOT" != "$WANT" ]; then
  echo "rust-toolchain: FAIL — rustc is ${GOT}, but ${TOML} pins ${WANT}." >&2
  echo "  RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-<unset>}" >&2
  echo "  An environment toolchain outranks the file's directory override, so" >&2
  echo "  the pin is not in effect. Pass the pinned version to the toolchain" >&2
  echo "  action's \`toolchain:\` input, or bump the file — but do not ship a" >&2
  echo "  release built by an unrecorded compiler." >&2
  exit 1
fi

echo "rust-toolchain: OK — rustc ${GOT} matches the pinned channel in ${TOML}."
