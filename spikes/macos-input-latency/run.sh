#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The PortalSwitch Authors
# Build, bundle, sign and run the latency spike as a real .app so macOS attributes the
# permissions to the app (not to the terminal).
#
# Usage: spikes/macos-input-latency/run.sh <mode> [args...]
#   modes: check | synthetic <hz> <secs> | real <secs>
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
APP="$HOME/Applications/PortalSwitch Latency Spike.app"
BUNDLE_ID="dev.portalswitch.spike.latency"

cargo build --release --quiet --manifest-path "$HERE/Cargo.toml"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$HERE/target/release/macos-input-latency" "$APP/Contents/MacOS/"
cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleName</key><string>PortalSwitch Latency Spike</string>
  <key>CFBundleExecutable</key><string>macos-input-latency</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.0.0</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
"$ROOT/dev/macos-signing.sh" sign "$APP" >/dev/null

open -n -W "$APP" --args "$@"
