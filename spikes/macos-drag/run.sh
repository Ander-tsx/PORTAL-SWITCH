#!/usr/bin/env bash
# Build, bundle, sign and run the drag spike as a real .app (needed for drop-synthetic, which
# posts mouse events and therefore requires Accessibility for this bundle).
#
# Usage: spikes/macos-drag/run.sh <mode> [args...]
#   modes: detect <secs> | drop-window <secs> | drop-synthetic <x> <y>
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
APP="$HOME/Applications/PortalSwitch Drag Spike.app"
BUNDLE_ID="dev.portalswitch.spike.drag"

cargo build --release --quiet --manifest-path "$HERE/Cargo.toml"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$HERE/target/release/macos-drag" "$APP/Contents/MacOS/"
cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleName</key><string>PortalSwitch Drag Spike</string>
  <key>CFBundleExecutable</key><string>macos-drag</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.0.0</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
"$ROOT/dev/macos-signing.sh" sign "$APP" >/dev/null

open -n -W "$APP" --args "$@"
tail -20 "$HOME/Library/Logs/portalswitch-spike/drag.log"
