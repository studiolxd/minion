#!/usr/bin/env bash
# Builds Minion.app, a self-contained macOS application bundle.
#
# Bundling matters for more than tidiness: macOS attributes microphone and
# Accessibility permissions to the binary that asks. Run from a terminal and
# the permission belongs to the terminal — meaning every script you run
# inherits it. Bundled, the permission is Minion's alone and shows up under
# its own name in System Settings.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="$HERE/Minion.app"
# One version, read from Cargo.toml — the crate version is the only place
# it is written down, so `minion --version`, the bundle's Info.plist and a
# release's tag cannot drift apart.
VERSION=$(awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' "$HERE/Cargo.toml")
[ -n "$VERSION" ] || { echo "Could not read the version from Cargo.toml" >&2; exit 1; }

cd "$HERE"

echo "Building…"
cargo build --release

echo "Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp target/release/minion "$APP/Contents/MacOS/minion"

# The models are not bundled: 670 MB that never change, downloaded once on
# first run into Application Support, where they also survive reinstalling.

# The app icon comes from the same drawing as the menu bar face, so the two
# cannot drift apart.
ICONSET="$HERE/target/Minion.iconset"
rm -rf "$ICONSET"
if ./target/release/minion export-icon "$ICONSET" >/dev/null 2>&1 \
   && iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Minion.icns" 2>/dev/null; then
  echo "Icon built."
else
  echo "Warning: could not build the icon; the app will use the generic one." >&2
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>Minion</string>
    <key>CFBundleDisplayName</key>     <string>Minion</string>
    <key>CFBundleIdentifier</key>      <string>com.studiolxd.minion</string>
    <key>CFBundleVersion</key>         <string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleExecutable</key>      <string>minion</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleIconFile</key>        <string>Minion</string>
    <key>LSMinimumSystemVersion</key>  <string>13.0</string>

    <!-- Menu bar only: no Dock icon, no window. -->
    <key>LSUIElement</key>             <true/>

    <key>NSMicrophoneUsageDescription</key>
    <string>Minion escucha para reconocer las órdenes que le dices. El audio se procesa en tu Mac y no sale de él.</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>Minion controla aplicaciones para cumplir las órdenes que le das por voz.</string>
</dict>
</plist>
PLIST

# Signing identity decides whether permissions survive a rebuild.
#
# An ad-hoc signature ties the grant to the exact bytes of the binary, so
# every rebuild silently revokes Accessibility and the app goes back to
# being ignored by the window server — with no error anywhere. A real
# certificate ties it to the team and bundle id instead, which stay put.
IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
  | grep -m1 "Apple Development" | awk '{print $2}')

if [ -n "$IDENTITY" ]; then
  codesign --force --deep --sign "$IDENTITY" "$APP" \
    && echo "Signed with Apple Development ($IDENTITY)." \
    || { echo "Signing failed; falling back to ad-hoc." >&2; \
         codesign --force --deep --sign - "$APP"; }
else
  codesign --force --deep --sign - "$APP" 2>/dev/null \
    && echo "Signed (ad-hoc). Accessibility will need re-granting after each rebuild." \
    || echo "Warning: could not sign at all."
fi

echo
echo "Built $APP  ($(du -sh "$APP" | cut -f1))"
echo "Open it with:  open '$APP'"
echo "Install it with:  mv '$APP' /Applications/"
