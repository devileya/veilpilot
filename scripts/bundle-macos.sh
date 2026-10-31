#!/usr/bin/env bash
# Build VeilPilot.app (and optionally a .dmg) from the release binary.
#
#   scripts/bundle-macos.sh            # target/release/bundle/VeilPilot.app
#   scripts/bundle-macos.sh --dmg      # ...plus VeilPilot-<version>.dmg
#   SKIP_BUILD=1 scripts/bundle-macos.sh --dmg   # bundle target/release/veilpilot as is
#
# Signs ad-hoc unless CODESIGN_IDENTITY names a Developer ID certificate. An
# ad-hoc signature changes on every build, so macOS asks for Screen Recording
# and Microphone permission again after each rebuild; a real identity keeps
# the grants.
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=target/release/bundle
APP="$OUT/VeilPilot.app"

# CI builds a universal binary into target/release itself and sets SKIP_BUILD.
if [[ -z "${SKIP_BUILD:-}" ]]; then
  cargo build --release
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/veilpilot "$APP/Contents/MacOS/VeilPilot"

# The icon is drawn by the app itself, so there is no image asset to keep in sync.
ICONS=$(mktemp -d)
target/release/veilpilot --write-icons "$ICONS/png"
SET="$ICONS/VeilPilot.iconset"
mkdir -p "$SET"
for size in 16 32 128 256 512; do
  cp "$ICONS/png/icon_${size}.png" "$SET/icon_${size}x${size}.png"
  cp "$ICONS/png/icon_$((size * 2)).png" "$SET/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$SET" -o "$APP/Contents/Resources/VeilPilot.icns"
rm -rf "$ICONS"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>co.interopera.veilpilot</string>
  <key>CFBundleName</key><string>VeilPilot</string>
  <key>CFBundleDisplayName</key><string>VeilPilot</string>
  <key>CFBundleExecutable</key><string>VeilPilot</string>
  <key>CFBundleIconFile</key><string>VeilPilot</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <!-- Accessory app: no Dock icon, no menu bar, no App Switcher entry. -->
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key>
  <string>VeilPilot records your microphone only while you hold the transcribe hotkey, to turn speech into text.</string>
</dict>
</plist>
PLIST

if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP"
else
  codesign --force --sign - "$APP"
fi
codesign --verify --strict "$APP"
echo "built $APP"

if [[ "${1:-}" == "--dmg" ]]; then
  DMG="$OUT/VeilPilot-${VERSION}.dmg"
  rm -f "$DMG"
  hdiutil create -volname VeilPilot -srcfolder "$APP" -ov -format UDZO "$DMG" >/dev/null
  echo "built $DMG"
fi
