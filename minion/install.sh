#!/usr/bin/env bash
# Installs Minion into /Applications and starts it at login.
#
# Run ./build-app.sh first, or let this script do it.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="$HERE/Minion.app"
TARGET="/Applications/Minion.app"
AGENT="$HOME/Library/LaunchAgents/com.studiolxd.minion.plist"

[ -d "$APP" ] || "$HERE/build-app.sh"

echo "Installing to $TARGET"
if [ -d "$TARGET" ]; then
  # Quit the running copy, or the replace fails with "file busy".
  osascript -e 'quit app "Minion"' 2>/dev/null || true
  sleep 1
  rm -rf "$TARGET"
fi
cp -R "$APP" "$TARGET"

echo "Installing the launch agent"
mkdir -p "$(dirname "$AGENT")"
cat > "$AGENT" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>            <string>com.studiolxd.minion</string>
    <key>ProgramArguments</key>
    <array>
        <string>$TARGET/Contents/MacOS/minion</string>
    </array>
    <key>RunAtLoad</key>        <true/>
    <!-- Restart if it ever crashes, but not in a tight loop. -->
    <key>KeepAlive</key>
    <dict><key>SuccessfulExit</key><false/></dict>
    <key>ThrottleInterval</key> <integer>10</integer>
    <!-- Not minion.log: Minion writes every line there itself, and
         pointing stdout at the same file printed each line twice. -->
    <key>StandardOutPath</key>  <string>$HOME/Library/Logs/minion-launch.log</string>
    <key>StandardErrorPath</key><string>$HOME/Library/Logs/minion-launch.log</string>
</dict>
</plist>
PLIST

launchctl bootout "gui/$(id -u)/com.studiolxd.minion" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$AGENT"

echo
echo "Installed. Minion is running and will start at login."
echo "Log:       ~/Library/Logs/minion.log"
echo "Uninstall: ./uninstall.sh"
