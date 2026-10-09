#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The PortalSwitch Authors
#
# Builds PortalSwitch.app (the UI window) for local testing on macOS.
#   ui/shell/bundle.sh [output-dir]      default output: ~/Applications
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="${1:-$HOME/Applications}"
app="$out/PortalSwitch.app"

echo "• building the interface"
(cd "$root/ui" && npm run build >/dev/null)

echo "• building the window shell"
(cd "$here" && cargo build --release)

echo "• assembling $app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$here/target/release/portalswitch-shell" "$app/Contents/MacOS/PortalSwitch"

iconset="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$here/assets/AppIcon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" "$here/assets/AppIcon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>PortalSwitch</string>
  <key>CFBundleDisplayName</key><string>PortalSwitch</string>
  <key>CFBundleIdentifier</key><string>app.portalswitch.ui</string>
  <key>CFBundleExecutable</key><string>PortalSwitch</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.0.0</string>
  <key>CFBundleVersion</key><string>0</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

echo "• signing with the local development identity"
"$root/dev/macos-signing.sh" sign "$app"
echo "done: $app   (open it with: open \"$app\")"
