#!/usr/bin/env bash
# Install aiui companion on the user's Mac.
# Run this FROM YOUR MAC after copying the zip there.
set -euo pipefail

ZIP="${1:-$HOME/Downloads/aiui-0.1.0-arm64.zip}"
if [[ ! -f "$ZIP" ]]; then
  echo "Zip not found: $ZIP" >&2
  echo "Usage: $0 [path-to-zip]" >&2
  exit 1
fi

echo "→ Unzipping to /tmp ..."
rm -rf /tmp/aiui-install && mkdir -p /tmp/aiui-install
ditto -x -k "$ZIP" /tmp/aiui-install

if [[ -d /Applications/aiui.app ]]; then
  echo "→ Replacing existing /Applications/aiui.app ..."
  rm -rf /Applications/aiui.app
fi
mv /tmp/aiui-install/aiui.app /Applications/

echo "→ Clearing quarantine flag (unsigned build) ..."
xattr -dr com.apple.quarantine /Applications/aiui.app || true

echo "→ Launching once to generate pairing token ..."
open -a /Applications/aiui.app
sleep 2

TOKEN_PATH="$HOME/.config/aiui/token"
if [[ -f "$TOKEN_PATH" ]]; then
  echo
  echo "✓ Installed. Token at: $TOKEN_PATH"
  echo
  echo "Next steps:"
  echo "  1) Add to ~/Library/Application Support/Claude/claude_desktop_config.json:"
  echo '     "aiui-local": { "command": "/Applications/aiui.app/Contents/MacOS/aiui", "args": ["--mcp-stdio"] }'
  echo "  2) Add to ~/.ssh/config for your remote host:"
  echo "     RemoteForward 7777 localhost:7777"
  echo "  3) Copy token to remote:"
  echo "     ssh <remote> 'mkdir -p ~/.config/aiui' && scp '$TOKEN_PATH' <remote>:~/.config/aiui/token"
  echo "  4) Restart Claude Desktop."
else
  echo "! Token file not created — open /Applications/aiui.app manually once."
fi
