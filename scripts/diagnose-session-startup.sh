#!/usr/bin/env bash
# Diagnose helper for "Claude Code sessions are slow to start since aiui was added".
#
# Two strategies, run them in this order:
#
#   ./diagnose-session-startup.sh bench
#       Times the MCP init handshake (initialize → tools/list → prompts/list)
#       for every MCP server in ~/.claude.json. Pure handshake latency, no
#       interaction with Claude Code itself. Tells you which MCP — if any —
#       is slow at startup. aiui's Rust MCP should be sub-second.
#
#   ./diagnose-session-startup.sh toggle-aiui
#       Removes aiui from ~/.claude.json (backup kept). Run a fresh Claude
#       Code session, observe startup time. Run the script again to restore.
#       Compare A/B by feel — if you can't tell with-vs-without apart, aiui
#       is not the bottleneck.
#
# Pragmatically: do `bench` first. If aiui is fast there, fully exclude it
# with `toggle-aiui` and confirm the slow start persists without aiui.

set -euo pipefail

CONFIG="${HOME}/.claude.json"
# Only the removed `aiui` entry is stashed here, never a whole-file copy:
# restoring a stale copy would throw away everything Claude Code wrote
# during the measurement window this script explicitly asks you to create
# (#182). The stash is a single JSON object.
STASH="${HOME}/.claude.json.aiui-entry"

cmd="${1:-help}"

bench_one() {
  local name="$1" command="$2" args_json="$3"

  # Build argv as a bash array from the JSON args list. Tab-separates so
  # whitespace inside args survives.
  local IFS=$'\t'
  local args
  args=($(printf '%s' "$args_json" | python3 -c '
import json, sys
arr = json.load(sys.stdin)
print("\t".join(arr))
'))
  unset IFS

  local probes
  probes=$(printf '%s\n' \
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"diagnose","version":"1"}}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
    '{"jsonrpc":"2.0","id":3,"method":"prompts/list"}' \
  )

  local start_ms end_ms elapsed
  start_ms=$(python3 -c 'import time; print(int(time.time()*1000))')

  # Pipe the probes in; let the server run for up to 10s; capture stdout.
  local out
  out=$(
    {
      printf '%s\n' "$probes"
      sleep 0.5
    } | "$command" "${args[@]}" 2>/dev/null \
      || true
  )

  end_ms=$(python3 -c 'import time; print(int(time.time()*1000))')
  elapsed=$((end_ms - start_ms))

  local n_lines tools_n prompts_n
  n_lines=$(printf '%s\n' "$out" | grep -c '^{' || true)
  tools_n=$(printf '%s' "$out" | python3 -c '
import json, sys
n = 0
for line in sys.stdin.read().splitlines():
    if not line.startswith("{"): continue
    try:
        obj = json.loads(line)
    except Exception:
        continue
    if obj.get("id") == 2 and "result" in obj:
        n = len(obj["result"].get("tools", []))
        break
print(n)
' 2>/dev/null || echo 0)
  prompts_n=$(printf '%s' "$out" | python3 -c '
import json, sys
n = 0
for line in sys.stdin.read().splitlines():
    if not line.startswith("{"): continue
    try:
        obj = json.loads(line)
    except Exception:
        continue
    if obj.get("id") == 3 and "result" in obj:
        n = len(obj["result"].get("prompts", []))
        break
print(n)
' 2>/dev/null || echo 0)

  printf '  %-25s %6d ms   tools=%d prompts=%d responses=%d\n' \
    "$name" "$elapsed" "$tools_n" "$prompts_n" "$n_lines"
}

bench_all() {
  if [[ ! -f "$CONFIG" ]]; then
    echo "no $CONFIG — nothing to bench" >&2
    exit 1
  fi
  echo "Benching MCP handshakes from $CONFIG ..."
  echo "(low ms = fast init; sub-second is healthy)"
  echo

  python3 -c "
import json
d = json.load(open('$CONFIG'))
for name, cfg in (d.get('mcpServers') or {}).items():
    cmd = cfg.get('command', '')
    args = json.dumps(cfg.get('args', []))
    print(f'{name}\t{cmd}\t{args}')
" | while IFS=$'\t' read -r name cmd args_json; do
    if [[ -z "$cmd" ]]; then
      printf '  %-25s skipped (no command)\n' "$name"
      continue
    fi
    if ! command -v "$cmd" >/dev/null 2>&1 && [[ ! -x "$cmd" ]]; then
      printf '  %-25s skipped (command not found: %s)\n' "$name" "$cmd"
      continue
    fi
    bench_one "$name" "$cmd" "$args_json"
  done

  echo
  echo "Read: anything > 2000 ms is suspicious. If aiui is fast here but"
  echo "sessions still feel slow, run \`$0 toggle-aiui\` and compare."
}

toggle_aiui() {
  if [[ ! -f "$CONFIG" ]]; then
    echo "no $CONFIG to toggle" >&2
    exit 1
  fi

  if [[ -f "$STASH" ]]; then
    echo "Restoring the aiui entry in $CONFIG (stashed at $STASH)"
    CONFIG="$CONFIG" STASH="$STASH" python3 - <<'PYEOF'
import json, os, sys
p, stash = os.environ["CONFIG"], os.environ["STASH"]
# Re-read the CURRENT config: Claude Code has been writing to it for the whole
# measurement window. Only the one key we removed goes back in.
try:
    d = json.load(open(p))
except Exception as e:
    sys.exit(f"{p} is not valid JSON ({e}) — fix it first; stash kept at {stash}")
entry = json.load(open(stash))
servers = d.get("mcpServers") or {}
servers["aiui"] = entry
d["mcpServers"] = servers
tmp = p + ".aiui-tmp"
with open(tmp, "w") as f:
    json.dump(d, f, indent=2)
os.replace(tmp, p)
os.unlink(stash)
print("mcpServers now:", list(servers.keys()))
PYEOF
    return
  fi

  CONFIG="$CONFIG" STASH="$STASH" python3 - <<'PYEOF'
import json, os, sys
p, stash = os.environ["CONFIG"], os.environ["STASH"]
try:
    d = json.load(open(p))
except Exception as e:
    sys.exit(f"{p} is not valid JSON ({e}) — refusing to touch it")
servers = d.get("mcpServers") or {}
entry = servers.pop("aiui", None)
if entry is None:
    sys.exit("no top-level `aiui` entry in mcpServers — nothing to toggle")
with open(stash, "w") as f:
    json.dump(entry, f, indent=2)
d["mcpServers"] = servers
tmp = p + ".aiui-tmp"
with open(tmp, "w") as f:
    json.dump(d, f, indent=2)
os.replace(tmp, p)
print("removed: True")
print("remaining mcpServers:", list(servers.keys()))
# A per-project entry would keep aiui loaded for the project you are about to
# measure, making the A/B read "aiui is not the bottleneck" for the wrong
# reason.
per_project = [
    k for k, v in (d.get("projects") or {}).items()
    if isinstance(v, dict) and "aiui" in (v.get("mcpServers") or {})
]
if per_project:
    print()
    print("WARNING: a per-project aiui entry still exists for:")
    for k in per_project:
        print(f"  {k}")
    print("aiui stays loaded for those projects — measure a different one,")
    print("or remove those entries by hand as well.")
PYEOF
  echo
  echo "Entry stashed at $STASH. Now: start a fresh Claude Code session, gauge startup."
  echo "When done, run this command again to restore."
}

case "$cmd" in
  bench)        bench_all ;;
  toggle-aiui)  toggle_aiui ;;
  *)
    cat <<EOF
usage: $0 {bench|toggle-aiui}

  bench         time the MCP init handshake for every MCP server in
                ~/.claude.json (pure handshake, no Claude Code interaction)
  toggle-aiui   remove aiui from ~/.claude.json (backup kept) for an A/B
                comparison of session-start time. Run again to restore.
EOF
    ;;
esac
