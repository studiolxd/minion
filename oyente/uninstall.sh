#!/usr/bin/env bash
# Removes Oyente: the launch agent, then the application.
set -euo pipefail

AGENT="$HOME/Library/LaunchAgents/com.studiolxd.oyente.plist"

launchctl bootout "gui/$(id -u)/com.studiolxd.oyente" 2>/dev/null || true
rm -f "$AGENT"
osascript -e 'quit app "Oyente"' 2>/dev/null || true
sleep 1
rm -rf /Applications/Oyente.app

echo "Removed. The speech model in the project folder is untouched."
echo "Microphone and Accessibility permissions can be revoked in System Settings."
