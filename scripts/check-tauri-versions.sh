#!/usr/bin/env bash
#
# check-tauri-versions.sh — the npm `@tauri-apps/*` packages must sit on the
# same major.minor as their Rust crates.
#
# The Tauri CLI refuses to bundle otherwise ("Found version mismatched Tauri
# packages") — but only CI's Windows leg runs `tauri build`, ~10 minutes into
# a macOS+Windows run, and the release workflows only at release time. A
# caret range let `npm install` pull @tauri-apps/api 2.12 and
# @tauri-apps/plugin-dialog 2.8 against tauri 2.10 / tauri-plugin-dialog 2.7.
# This light check reads the two lockfiles and fails in seconds on Linux.
#
# Pairs: @tauri-apps/api ↔ tauri; @tauri-apps/plugin-<x> ↔ tauri-plugin-<x>.
# A nested copy (node_modules/@tauri-apps/<p>/node_modules/@tauri-apps/…)
# also fails: it means a plugin's JS needs a newer api than the crate.
#
# Run locally:  scripts/check-tauri-versions.sh [package-lock.json Cargo.lock]
# CI:           .github/workflows/ci.yml -> job `skill-drift`

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NPM_LOCK="${1:-$ROOT/companion/package-lock.json}"
CARGO_LOCK="${2:-$ROOT/companion/src-tauri/Cargo.lock}"

python3 - "$NPM_LOCK" "$CARGO_LOCK" <<'PY'
import json, re, sys
npm_lock, cargo_lock = sys.argv[1], sys.argv[2]
pkgs = json.load(open(npm_lock))["packages"]
crates = {}
for m in re.finditer(r'\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"', open(cargo_lock).read()):
    crates.setdefault(m.group(1), m.group(2))
def mm(v): return ".".join(v.split(".")[:2])
problems, checked = [], 0
for key, meta in pkgs.items():
    if not key.startswith("node_modules/@tauri-apps/"):
        continue
    if key.count("node_modules/") > 1:
        problems.append(f"nested {key} {meta.get('version')} — a plugin's JS needs a newer @tauri-apps/api than the tauri crate")
        continue
    name = key[len("node_modules/@tauri-apps/"):]
    crate = "tauri" if name == "api" else ("tauri-" + name if name.startswith("plugin-") else None)
    if crate is None or crate not in crates:
        continue  # the CLI and its platform binaries pair with no crate
    checked += 1
    if mm(meta["version"]) != mm(crates[crate]):
        problems.append(f"@tauri-apps/{name} {meta['version']} vs {crate} {crates[crate]} — major.minor must match")
if problems:
    print("tauri-versions: FAIL —", file=sys.stderr)
    for p in problems:
        print(f"  - {p}", file=sys.stderr)
    sys.exit(1)
print(f"tauri-versions: OK — {checked} @tauri-apps package(s) match their crates' major.minor.")
PY
