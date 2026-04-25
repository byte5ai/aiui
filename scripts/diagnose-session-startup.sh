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
BACKUP="${HOME}/.claude.json.aiui-toggle-backup"

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

  if [[ -f "$BACKUP" ]]; then
    echo "Restoring aiui in $CONFIG (backup found at $BACKUP)"
    mv "$BACKUP" "$CONFIG"
    python3 -c "
import json
d = json.load(open('$CONFIG'))
print('mcpServers now:', list((d.get('mcpServers') or {}).keys()))
"
    return
  fi

  cp "$CONFIG" "$BACKUP"
  python3 -c "
import json
p = '$CONFIG'
d = json.load(open(p))
servers = d.get('mcpServers') or {}
removed = servers.pop('aiui', None)
d['mcpServers'] = servers
json.dump(d, open(p, 'w'), indent=2)
print('removed:', removed is not None)
print('remaining mcpServers:', list(servers.keys()))
"
  echo
  echo "Backup at $BACKUP. Now: start a fresh Claude Code session, gauge startup."
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
