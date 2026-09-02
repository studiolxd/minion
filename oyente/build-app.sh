#!/usr/bin/env bash
# Builds Oyente.app, a self-contained macOS application bundle.
#
# Bundling matters for more than tidiness: macOS attributes microphone and
# Accessibility permissions to the binary that asks. Run from a terminal and
# the permission belongs to the terminal — meaning every script you run
# inherits it. Bundled, the permission is Oyente's alone and shows up under
# its own name in System Settings.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="$HERE/Oyente.app"
VERSION="1.0.0-beta.1"

cd "$HERE"

if [ ! -f model/vocab.txt ]; then
  echo "Speech model missing. Run ./download-model.sh first." >&2
  exit 1
fi

echo "Building…"
cargo build --release

echo "Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp target/release/oyente "$APP/Contents/MacOS/oyente"
cp -R model "$APP/Contents/Resources/model"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>Oyente</string>
    <key>CFBundleDisplayName</key>     <string>Oyente</string>
    <key>CFBundleIdentifier</key>      <string>com.studiolxd.oyente</string>
    <key>CFBundleVersion</key>         <string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleExecutable</key>      <string>oyente</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>LSMinimumSystemVersion</key>  <string>13.0</string>

    <!-- Menu bar only: no Dock icon, no window. -->
    <key>LSUIElement</key>             <true/>

    <key>NSMicrophoneUsageDescription</key>
    <string>Oyente escucha para reconocer las órdenes que le dices. El audio se procesa en tu Mac y no sale de él.</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>Oyente controla aplicaciones para cumplir las órdenes que le das por voz.</string>
</dict>
</plist>
PLIST

# Ad-hoc signature. Enough for macOS to keep permissions attached to this
# bundle across rebuilds; a Developer ID would be needed to distribute it.
codesign --force --deep --sign - "$APP" 2>/dev/null \
  && echo "Signed (ad-hoc)." \
  || echo "Warning: could not sign; permissions may reset on each rebuild."

echo
echo "Built $APP  ($(du -sh "$APP" | cut -f1))"
echo "Open it with:  open '$APP'"
echo "Install it with:  mv '$APP' /Applications/"
