#!/usr/bin/env bash
#
# check-skill-drift.sh — guard the agent-facing contract: the two shipped
# copies of the aiui skill must document the same surface, and that surface
# must match what the two bridges actually accept.
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
# Python copy). Three separate assertions:
#
#   1. check_header_tokens — the *sections* must match. Every backtick-
#      quoted identifier in a level 2-4 Markdown header (`## `, `### `,
#      `#### `) of one copy must appear in a header of the other, minus the
#      allowlists. Catches "canon grew a section, the Python copy didn't".
#
#   2. check_field_kinds — the *field kinds* must be documented. The list
#      is parsed out of `KNOWN_FIELD_KINDS` in
#      companion/src-tauri/src/http.rs — the validator that decides which
#      kinds a form may use — and every kind must appear as a backticked
#      token somewhere in BOTH copies (not only in a header). #205: `tree`
#      shipped fully implemented and validated while being absent from both
#      catalogs, so no agent could ever reach it. A header-only check can
#      never see that, because a missing section is missing symmetrically.
#
#   3. check_tool_parity — the *tools* must match across bridges. The
#      `tools_list()` name set in companion/src-tauri/src/mcp.rs must equal
#      the `@mcp.tool()` name set in python/src/aiui_mcp/server.py (the
#      decorator's `name=` wins where present), and every dialog tool must
#      appear backticked in both copies.
#
# Checks 2 and 3 parse the Rust/Python sources on purpose. A list
# hardcoded here would drift in exactly the way this guard exists to catch.
#
# HOW TO ALLOW A DELIBERATE DIVERGENCE
# ------------------------------------
# When a token legitimately belongs to only one copy (a bridge-specific
# knob, or wording that only the canonical copy spells out), add it to the
# matching allowlist array and leave a one-line comment saying why. Same
# for a field kind or tool that deliberately stays out of the catalog. Keep
# the allowlists SHORT — every entry is a capability the agent can't see,
# or one the two copies disagree on, so each one needs a real reason.
#
# Run locally:  scripts/check-skill-drift.sh
# Self-test:    scripts/test-check-skill-drift.sh
# CI:           .github/workflows/ci.yml -> job `skill-drift`

set -euo pipefail

# Resolve repo root from this script's location so it works from anywhere.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CANON="$ROOT/docs/skill.md"
COPY="$ROOT/python/src/aiui_mcp/skill.md"
HTTP_RS="$ROOT/companion/src-tauri/src/http.rs"
MCP_RS="$ROOT/companion/src-tauri/src/mcp.rs"
SERVER_PY="$ROOT/python/src/aiui_mcp/server.py"

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

# Field kinds from KNOWN_FIELD_KINDS that neither copy has to name. Every
# entry is a widget an agent cannot discover, so each needs a reason.
#   number / checkbox / slider / color — plain single-value inputs whose
#     whole spec is {kind, name, label, default?}; they are listed in the
#     `form` tool description an agent always sees, and the catalog spends
#     its space on the fields that need judgement. Giving them a short
#     "plain input fields" section would be an improvement, not a fix.
ALLOW_UNDOCUMENTED_KINDS=(
  number
  checkbox
  slider
  color
)

# Tools that exist on both bridges but deliberately stay out of the
# catalog — they render no dialog, so there is no design guidance to give.
#   aiui_health — reachability/token diagnostics.
#   version     — reports companion version and build info.
#   update      — installs a companion update.
ALLOW_UNDOCUMENTED_TOOLS=(
  aiui_health
  version
  update
)

require_file() {
  if [ ! -f "$1" ]; then
    echo "ERROR: expected file not found: $1" >&2
    exit 2
  fi
}

# True when $1 contains $2 as a backtick-quoted token, anywhere in the file.
documents_token() { # file token
  grep -qF -- "\`$2\`" "$1"
}

# --- 1. section (header token) parity -------------------------------------

# Extract the set of backtick-quoted tokens found in level 2-4 headers.
extract_tokens() {
  grep -E '^#{2,4} ' "$1" \
    | grep -oE '`[^`]+`' \
    | tr -d '`' \
    | sort -u
}

check_header_tokens() {
  local canon_tokens copy_tokens allow_canon_only allow_copy_only
  local missing_in_copy missing_in_canon rc=0

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

  if [ -n "$missing_in_copy" ]; then
    rc=1
    echo "ERROR: field/tool sections in docs/skill.md but NOT in python/src/aiui_mcp/skill.md:"
    printf '  - %s\n' $missing_in_copy
    echo "  -> add the matching section to the Python copy, or allowlist it in ALLOW_CANON_ONLY."
  fi

  if [ -n "$missing_in_canon" ]; then
    rc=1
    echo "ERROR: field/tool sections in python/src/aiui_mcp/skill.md but NOT in docs/skill.md:"
    printf '  - %s\n' $missing_in_canon
    echo "  -> add the matching section to docs/skill.md, or allowlist it in ALLOW_COPY_ONLY."
  fi

  return "$rc"
}

# --- 2. field-kind coverage ------------------------------------------------

# The kinds a form may actually use, straight out of the Rust validator.
known_field_kinds() {
  sed -n '/const KNOWN_FIELD_KINDS/,/];/p' "$HTTP_RS" \
    | grep -oE '"[a-z_0-9]+"' \
    | tr -d '"' \
    | sort -u
}

check_field_kinds() {
  local kinds kind rc=0
  local -a undocumented=()

  kinds="$(known_field_kinds)"
  if [ -z "$kinds" ]; then
    echo "ERROR: could not parse KNOWN_FIELD_KINDS out of companion/src-tauri/src/http.rs."
    echo "  -> the const moved or changed shape; fix known_field_kinds() in this script."
    return 1
  fi

  for kind in $kinds; do
    if printf '%s\n' "${ALLOW_UNDOCUMENTED_KINDS[@]:-}" | grep -qxF -- "$kind"; then
      continue
    fi
    if ! documents_token "$CANON" "$kind" || ! documents_token "$COPY" "$kind"; then
      undocumented+=("$kind")
    fi
  done

  if [ "${#undocumented[@]}" -gt 0 ]; then
    rc=1
    echo "ERROR: field kinds accepted by KNOWN_FIELD_KINDS but not documented in both skill.md copies:"
    printf '  - %s\n' "${undocumented[@]}"
    echo "  -> an agent cannot use a field it never reads about. Add a section to"
    echo "     docs/skill.md AND python/src/aiui_mcp/skill.md (spec, when-to-use, one"
    echo "     example, one anti-pattern), or allowlist it in ALLOW_UNDOCUMENTED_KINDS"
    echo "     with a one-line reason."
  fi

  return "$rc"
}

# --- 3. tool parity across the two bridges --------------------------------

# Tool names from `fn tools_list()` in the Rust MCP server. Bounded to that
# function's body on purpose: `serverInfo` and `prompts_list()` carry
# "name" keys too and would otherwise be picked up as tools.
rust_tool_names() {
  sed -n '/^fn tools_list/,/^}/p' "$MCP_RS" \
    | grep -oE '"name": "[a-z_0-9]+"' \
    | sed -E 's/^"name": "(.*)"$/\1/' \
    | sort -u
}

# Tool names from the FastMCP decorators in the Python bridge. Two tools are
# registered under a name that differs from the function name
# (`@mcp.tool(name="version")`, `@mcp.tool(name="update")`), so the
# decorator's `name=` wins where present.
python_tool_names() {
  awk '
    /^@mcp\.tool\(/ {
      if (match($0, /name="[a-z_0-9]+"/)) {
        print substr($0, RSTART + 6, RLENGTH - 7)
        pending = 0
      } else {
        pending = 1
      }
      next
    }
    pending && /^(async )?def [a-z_0-9]+/ {
      name = $0
      sub(/^(async )?def /, "", name)
      sub(/\(.*/, "", name)
      print name
      pending = 0
    }
  ' "$SERVER_PY" | sort -u
}

check_tool_parity() {
  local rust_tools py_tools only_rust only_python tool rc=0
  local -a undocumented=()

  rust_tools="$(rust_tool_names)"
  py_tools="$(python_tool_names)"

  if [ -z "$rust_tools" ]; then
    echo "ERROR: could not parse tool names out of tools_list() in companion/src-tauri/src/mcp.rs."
    echo "  -> fix rust_tool_names() in this script."
    return 1
  fi
  if [ -z "$py_tools" ]; then
    echo "ERROR: could not parse @mcp.tool() names out of python/src/aiui_mcp/server.py."
    echo "  -> fix python_tool_names() in this script."
    return 1
  fi

  only_rust="$(comm -23 <(printf '%s\n' "$rust_tools") <(printf '%s\n' "$py_tools"))"
  only_python="$(comm -13 <(printf '%s\n' "$rust_tools") <(printf '%s\n' "$py_tools"))"

  if [ -n "$only_rust" ]; then
    rc=1
    echo "ERROR: tools served by the Rust bridge (tools_list) but NOT by the Python bridge:"
    printf '  - %s\n' $only_rust
    echo "  -> a remote-SSH agent silently loses a capability a local one has."
  fi

  if [ -n "$only_python" ]; then
    rc=1
    echo "ERROR: tools served by the Python bridge (@mcp.tool) but NOT by the Rust bridge:"
    printf '  - %s\n' $only_python
    echo "  -> a local agent silently loses a capability a remote one has."
  fi

  # Every dialog tool needs guidance in both catalogs. Plumbing tools are
  # allowlisted above.
  for tool in $rust_tools; do
    if printf '%s\n' "${ALLOW_UNDOCUMENTED_TOOLS[@]:-}" | grep -qxF -- "$tool"; then
      continue
    fi
    if ! documents_token "$CANON" "$tool" || ! documents_token "$COPY" "$tool"; then
      undocumented+=("$tool")
    fi
  done

  if [ "${#undocumented[@]}" -gt 0 ]; then
    rc=1
    echo "ERROR: dialog tools not documented in both skill.md copies:"
    printf '  - %s\n' "${undocumented[@]}"
    echo "  -> document the tool in docs/skill.md AND python/src/aiui_mcp/skill.md,"
    echo "     or allowlist it in ALLOW_UNDOCUMENTED_TOOLS with a one-line reason."
  fi

  return "$rc"
}

# --- run -------------------------------------------------------------------

for f in "$CANON" "$COPY" "$HTTP_RS" "$MCP_RS" "$SERVER_PY"; do
  require_file "$f"
done

status=0
check_header_tokens || status=1
check_field_kinds || status=1
check_tool_parity || status=1

if [ "$status" -eq 0 ]; then
  echo "skill-drift: OK — both skill.md copies document the same field/tool surface,"
  echo "             every KNOWN_FIELD_KINDS kind is documented, and the two bridges"
  echo "             serve the same tools."
fi

exit "$status"
