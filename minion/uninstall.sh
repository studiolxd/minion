#!/usr/bin/env bash
# Removes Minion: the launch agent, then the application.
#
# By default this leaves your data behind — the voice profile, any saved
# recordings, config.toml, and the logs — the same way quitting an app
# does not erase its documents. Pass --purge to remove that too.
set -euo pipefail

AGENT="$HOME/Library/LaunchAgents/com.studiolxd.minion.plist"
APP_SUPPORT="$HOME/Library/Application Support/Minion"
LOG_GLOB="$HOME/Library/Logs/minion*.log*"

launchctl bootout "gui/$(id -u)/com.studiolxd.minion" 2>/dev/null || true
rm -f "$AGENT"
osascript -e 'quit app "Minion"' 2>/dev/null || true
sleep 1
rm -rf /Applications/Minion.app

echo "Removed the app and the launch agent."
echo
echo "Still on disk (not removed):"
echo "  $APP_SUPPORT"
echo "    — config.toml, voice.txt (your voice profile), the speech model,"
echo "      and recordings if you had save_recordings on"
for log in $HOME/Library/Logs/minion*.log*; do
  [ -e "$log" ] && echo "  $log"
done
echo
echo "Microphone and Accessibility permissions can be revoked in System Settings."

if [ "${1:-}" = "--purge" ]; then
  echo
  echo "--purge will permanently delete:"
  echo "  $APP_SUPPORT"
  echo "  $LOG_GLOB"
  read -r -p "Type 'yes' to confirm: " confirmation
  if [ "$confirmation" = "yes" ]; then
    rm -rf "$APP_SUPPORT"
    rm -f $HOME/Library/Logs/minion*.log*
    echo "Purged."
  else
    echo "Not confirmed — nothing further removed."
  fi
fi
