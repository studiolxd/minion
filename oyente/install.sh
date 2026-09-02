#!/usr/bin/env bash
# Installs Oyente into /Applications and starts it at login.
#
# Run ./build-app.sh first, or let this script do it.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="$HERE/Oyente.app"
TARGET="/Applications/Oyente.app"
AGENT="$HOME/Library/LaunchAgents/com.studiolxd.oyente.plist"

[ -d "$APP" ] || "$HERE/build-app.sh"

echo "Installing to $TARGET"
if [ -d "$TARGET" ]; then
  # Quit the running copy, or the replace fails with "file busy".
  osascript -e 'quit app "Oyente"' 2>/dev/null || true
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
    <key>Label</key>            <string>com.studiolxd.oyente</string>
    <key>ProgramArguments</key>
    <array>
        <string>$TARGET/Contents/MacOS/oyente</string>
    </array>
    <key>RunAtLoad</key>        <true/>
    <!-- Restart if it ever crashes, but not in a tight loop. -->
    <key>KeepAlive</key>
    <dict><key>SuccessfulExit</key><false/></dict>
    <key>ThrottleInterval</key> <integer>10</integer>
    <key>StandardOutPath</key>  <string>$HOME/Library/Logs/oyente.log</string>
    <key>StandardErrorPath</key><string>$HOME/Library/Logs/oyente.log</string>
</dict>
</plist>
PLIST

launchctl bootout "gui/$(id -u)/com.studiolxd.oyente" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$AGENT"

echo
echo "Installed. Oyente is running and will start at login."
echo "Log:       ~/Library/Logs/oyente.log"
echo "Uninstall: ./uninstall.sh"
