#!/usr/bin/env bash
# Put a pinned, checksum-verified rustup on a runner that has none — #291.
#
# The macOS jobs run on macrunner, a fresh VM per job whose image carries no
# Rust toolchain. `dtolnay/rust-toolchain` then falls back to
# `curl https://sh.rustup.rs | sh`: an unpinned installer, resolved at build
# time, in the job that later signs the release. Installing rustup here
# first — fixed version, SHA-256 checked — means the action finds it and
# skips its own download. A runner that already has rustup (GitHub-hosted
# Windows) is left alone.
#
# Bump: take the version from
# https://static.rust-lang.org/rustup/release-stable.toml and the digest from
# .../rustup/archive/<version>/<triple>/rustup-init.sha256 — then check the
# downloaded file against it yourself rather than copying the digest blind.

set -euo pipefail

RUSTUP_VERSION="1.29.2"

if command -v rustup >/dev/null 2>&1; then
  echo "install-rustup: rustup already on PATH ($(rustup --version 2>/dev/null | head -1 || true)) — nothing to install."
  exit 0
fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    TRIPLE="aarch64-apple-darwin"
    SHA256="f7c7e11eb2e07f006e532a8aaac77b32f4527d305ac386c0c633ca6133d35ca9"
    ;;
  *)
    echo "install-rustup: FAIL — no pinned rustup-init for $(uname -s)-$(uname -m); add its digest here." >&2
    exit 1
    ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl --proto '=https' --tlsv1.2 --retry 5 --retry-connrefused \
  --fail --silent --show-error --location \
  --output "$TMP/rustup-init" \
  "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${TRIPLE}/rustup-init"

echo "${SHA256}  $TMP/rustup-init" | shasum -a 256 -c - >/dev/null \
  || { echo "install-rustup: FAIL — rustup-init ${RUSTUP_VERSION} does not match the pinned SHA-256." >&2; exit 1; }

chmod +x "$TMP/rustup-init"
# No toolchain here: the workflow's toolchain step installs the pinned one.
"$TMP/rustup-init" -y --no-modify-path --default-toolchain none --profile minimal

CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$CARGO_BIN" >> "$GITHUB_PATH"
fi
echo "install-rustup: OK — $("$CARGO_BIN/rustup" --version 2>/dev/null | head -1 || true), SHA-256 verified."
