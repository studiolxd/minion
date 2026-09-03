#!/usr/bin/env bash
# Builds, signs, notarises and publishes a Minion release.
#
# build-app.sh makes a bundle that runs on *this* Mac: signed with the
# Apple Development certificate, which is enough to keep the Accessibility
# grant across rebuilds and nothing more. A copy handed to anyone else has
# to clear Gatekeeper, and that needs three things this script adds:
#
#   * a Developer ID Application signature, with the hardened runtime and a
#     secure timestamp (Apple refuses to notarise anything else),
#   * notarisation — Apple's own scan, which returns a ticket,
#   * that ticket stapled into the bundle, so the first launch works with
#     no network.
#
# Then it zips the result, hashes it, writes the appcast the in-app updater
# reads (see src/updater.rs) and publishes both as a GitHub release.
#
# Usage:
#   ./release.sh              # the real thing: notarise and publish
#   ./release.sh --dry-run    # build, sign, zip, hash, write the appcast;
#                             # no notarisation, no stapling, no upload
#   ./release.sh --notes "…"  # release notes, shown by the updater
#
# One-time setup for notarisation — stores an app-specific password in the
# login keychain under the profile name this script looks for:
#
#   xcrun notarytool store-credentials minion-notary \
#     --apple-id hello@studiolxd.com \
#     --team-id 28X5LFCFRQ \
#     --password <app-specific password from appleid.apple.com>
#
# Without that profile the script still builds and signs, says so, and
# stops before publishing: an un-notarised zip on a release page is worse
# than no release, since every download is quarantined and refused.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="$HERE/Minion.app"
DIST="$HERE/dist"
REPO="studiolxd/minion"
NOTARY_PROFILE="minion-notary"

DRY_RUN=0
NOTES=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --notes) NOTES="${2:-}"; shift ;;
    *) echo "Usage: $0 [--dry-run] [--notes \"…\"]" >&2; exit 2 ;;
  esac
  shift
done

cd "$HERE"

# The one place the version is written down; build-app.sh reads the same
# line for Info.plist.
VERSION=$(awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' Cargo.toml)
[ -n "$VERSION" ] || { echo "Could not read the version from Cargo.toml" >&2; exit 1; }
TAG="v$VERSION"
ZIP="$DIST/Minion-$VERSION.zip"
APPCAST="$DIST/appcast.json"

say() { printf '\n== %s\n' "$1"; }

say "Minion $VERSION${DRY_RUN:+ (dry run)}"

# ---------------------------------------------------------------- build
say "Building the bundle"
"$HERE/build-app.sh"

# --------------------------------------------------------------- signing
#
# Developer ID is the only identity Gatekeeper accepts on another Mac.
# Falling back to Apple Development keeps this script usable for a local
# dry run, but the result must never be published, so the warning is loud
# and the real run refuses to go on to notarisation without it.
# `|| true`: with no such certificate grep exits 1, and under `set -e` an
# assignment from a failing pipeline ends the script — silently, right
# after the build, which is when the warning below matters most.
IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
  | grep -m1 "Developer ID Application" | awk '{print $2}' || true)
DEVELOPER_ID=1
if [ -z "$IDENTITY" ]; then
  DEVELOPER_ID=0
  IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
    | grep -m1 "Apple Development" | awk '{print $2}' || true)
  echo "WARNING ============================================================" >&2
  echo "No 'Developer ID Application' certificate in the keychain." >&2
  echo "Signing with Apple Development instead. The result runs here and" >&2
  echo "NOWHERE ELSE: Gatekeeper will refuse it on any other Mac, and Apple" >&2
  echo "will not notarise it. Get a Developer ID certificate from" >&2
  echo "developer.apple.com before publishing anything." >&2
  echo "===================================================================" >&2
fi
[ -n "$IDENTITY" ] || { echo "No signing certificate at all; cannot continue." >&2; exit 1; }

# The hardened runtime is required for notarisation, and it switches off
# exactly the things Minion needs unless they are asked for by name: the
# microphone it listens through, and the Apple Events it drives other
# applications with.
ENTITLEMENTS="$HERE/target/minion.entitlements"
cat > "$ENTITLEMENTS" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.device.audio-input</key>       <true/>
    <key>com.apple.security.automation.apple-events</key>  <true/>
</dict>
</plist>
PLIST

say "Signing with $IDENTITY"
codesign --force --options runtime --timestamp \
  --entitlements "$ENTITLEMENTS" --sign "$IDENTITY" "$APP"
codesign --verify --strict --verbose=2 "$APP"

# ------------------------------------------------------------------ zip
#
# ditto -c -k --keepParent is what Apple documents for notarytool: a plain
# `zip` loses the symlinks and extended attributes a signature is made of,
# and the submission comes back invalid with nothing to read.
say "Zipping"
mkdir -p "$DIST"
rm -f "$ZIP"
/usr/bin/ditto -c -k --keepParent "$APP" "$ZIP"

# ----------------------------------------------------------- notarising
NOTARISED=0
if [ "$DRY_RUN" -eq 1 ]; then
  echo "Dry run: skipping notarisation and stapling."
elif [ "$DEVELOPER_ID" -eq 0 ]; then
  echo "Not notarising: the bundle is not signed with Developer ID." >&2
  echo "Stopping before publishing." >&2
  exit 1
elif ! xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1; then
  echo "No '$NOTARY_PROFILE' keychain profile. Create it once with:" >&2
  echo "  xcrun notarytool store-credentials $NOTARY_PROFILE \\" >&2
  echo "    --apple-id <apple id> --team-id <team> --password <app-specific>" >&2
  echo "Stopping before publishing." >&2
  exit 1
else
  say "Notarising (this takes a few minutes)"
  xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
  # The ticket is stapled into the bundle, not the zip, so the zip is
  # rebuilt afterwards — a download must carry the ticket with it.
  say "Stapling"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  rm -f "$ZIP"
  /usr/bin/ditto -c -k --keepParent "$APP" "$ZIP"
  NOTARISED=1
fi

# -------------------------------------------------------------- appcast
#
# What the in-app updater reads: the version to compare against, where the
# zip is, and the hash the download must produce before it is unpacked.
say "Writing the appcast"
SHA=$(/usr/bin/shasum -a 256 "$ZIP" | awk '{print $1}')
PUBLISHED=$(date -u +%Y-%m-%dT%H:%M:%SZ)
URL="https://github.com/$REPO/releases/download/$TAG/$(basename "$ZIP")"
[ -n "$NOTES" ] || NOTES="Minion $VERSION"
python3 - "$APPCAST" "$VERSION" "$URL" "$SHA" "$NOTES" "$PUBLISHED" <<'PY'
import json, sys
path, version, url, sha256, notes, published = sys.argv[1:7]
with open(path, "w") as f:
    json.dump(
        {"version": version, "url": url, "sha256": sha256,
         "notes": notes, "published": published},
        f, indent=2, ensure_ascii=False)
    f.write("\n")
PY
cat "$APPCAST"

# ------------------------------------------------------------ publishing
if [ "$DRY_RUN" -eq 1 ]; then
  say "Dry run finished"
  echo "Built and signed, not notarised, not published."
  echo "  bundle:  $APP"
  echo "  zip:     $ZIP"
  echo "  sha256:  $SHA"
  echo "  appcast: $APPCAST"
  exit 0
fi

[ "$NOTARISED" -eq 1 ] || { echo "Refusing to publish something un-notarised." >&2; exit 1; }

say "Publishing $TAG to $REPO"
gh release create "$TAG" "$ZIP" "$APPCAST" \
  --repo "$REPO" \
  --title "Minion $VERSION" \
  --notes "$NOTES"

say "Done"
echo "Released $TAG: $URL"
