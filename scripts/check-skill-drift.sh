#!/usr/bin/env bash
#
# check-skill-drift.sh — guard against accidental drift between the two
# shipped copies of the aiui agent skill.
#
# There are two skill.md files that MUST document the same field/tool
# surface, because both aiui bridges expose the same MCP tools:
#
#   docs/skill.md                  — canonical, embedded into the native
#                                    Rust MCP server via include_str!
#                                    (local Macs + Claude Code)
#   python/src/aiui_mcp/skill.md   — the copy shipped with the Python
#                                    bridge `aiui-mcp` (remote SSH hosts
#                                    via uvx)
#
# WHAT THIS CHECKS
# ----------------
# Not a byte-for-byte diff — the two copies are DELIBERATELY tailored per
# bridge (frontmatter/description, intro wording, terser prose in the
# Python copy). What must NOT drift is the *set of documented fields and
# tools*: if `docs/skill.md` grows a section for a new field or tool, the
# Python copy has to document it too (and vice versa), or a remote-SSH
# agent silently loses a capability it actually has.
#
# The guard extracts every backtick-quoted identifier that appears in a
# level 2-4 Markdown header (`## `, `### `, `#### `) of each file — these
# are the field/tool section markers (`notify`, `list`, `table`,
# `mermaid`, `wireframe`, `annotated_image`, `compare`, `image_grid`,
# `secret`, `datetime`, …) — and compares the two sets. It fails if either
# file documents a field/tool the other doesn't, minus the allowlist below.
#
# HOW TO ALLOW A DELIBERATE DIVERGENCE
# ------------------------------------
# When a token legitimately belongs to only one copy (a bridge-specific
# knob, or wording that only the canonical copy spells out), add it to the
# matching allowlist array and leave a one-line comment saying why. Keep
# the allowlist SHORT — every entry is a capability the two copies disagree
# on, so each one needs a real reason.
#
# Run locally:  scripts/check-skill-drift.sh
# CI:           .github/workflows/ci.yml -> job `skill-drift`

set -euo pipefail

# Resolve repo root from this script's location so it works from anywhere.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CANON="$ROOT/docs/skill.md"
COPY="$ROOT/python/src/aiui_mcp/skill.md"

# Tokens allowed to appear ONLY in the canonical docs/skill.md.
#   width / height — the canonical copy spells out the explicit
#     width/height size overrides in its section header; the Python copy
#     keeps a `size`-only header on purpose (the override is rarely used).
ALLOW_CANON_ONLY=(
  width
  height
)

# Tokens allowed to appear ONLY in the Python copy. (None today.)
ALLOW_COPY_ONLY=()

# Extract the set of backtick-quoted tokens found in level 2-4 headers.
extract_tokens() {
  grep -E '^#{2,4} ' "$1" \
    | grep -oE '`[^`]+`' \
    | tr -d '`' \
    | sort -u
}

canon_tokens="$(extract_tokens "$CANON")"
copy_tokens="$(extract_tokens "$COPY")"

# Build newline-delimited allowlists for grep -vxF filtering.
allow_canon_only="$(printf '%s\n' "${ALLOW_CANON_ONLY[@]}")"
allow_copy_only="$(printf '%s\n' "${ALLOW_COPY_ONLY[@]:-}")"

# In canon but not in copy (minus allowlist) => a section the copy is missing.
missing_in_copy="$(comm -23 <(printf '%s\n' "$canon_tokens") <(printf '%s\n' "$copy_tokens") \
  | grep -vxF -f <(printf '%s\n' "$allow_canon_only") || true)"

# In copy but not in canon (minus allowlist) => a section canon is missing.
missing_in_canon="$(comm -13 <(printf '%s\n' "$canon_tokens") <(printf '%s\n' "$copy_tokens") \
  | grep -vxF -f <(printf '%s\n' "$allow_copy_only") || true)"

status=0

if [ -n "$missing_in_copy" ]; then
  status=1
  echo "ERROR: field/tool sections in docs/skill.md but NOT in python/src/aiui_mcp/skill.md:"
  printf '  - %s\n' $missing_in_copy
  echo "  -> add the matching section to the Python copy, or allowlist it in ALLOW_CANON_ONLY."
fi

if [ -n "$missing_in_canon" ]; then
  status=1
  echo "ERROR: field/tool sections in python/src/aiui_mcp/skill.md but NOT in docs/skill.md:"
  printf '  - %s\n' $missing_in_canon
  echo "  -> add the matching section to docs/skill.md, or allowlist it in ALLOW_COPY_ONLY."
fi

if [ "$status" -eq 0 ]; then
  echo "skill-drift: OK — both skill.md copies document the same field/tool surface."
fi

exit "$status"
