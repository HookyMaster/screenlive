#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
ARCH="${1:-$(uname -m)}"
OUT="${2:-$PWD/dist}"
[[ "$ARCH" == arm64 || "$ARCH" == x86_64 ]] || exit 2
mkdir -p "$OUT/ScreenLive.app/Contents/MacOS"
APP="$OUT/ScreenLive.app"
xcrun swiftc -swift-version 5 -parse-as-library -module-cache-path "$OUT/module-cache" -O -target "$ARCH-apple-macosx15.0" \
  -sdk "$(xcrun --sdk macosx --show-sdk-path)" ScreenLive.swift CaptureViews.swift Camera.swift AudioGain.swift -o "$APP/Contents/MacOS/ScreenLive"
cp Info.plist "$APP/Contents/Info.plist"
mkdir -p "$APP/Contents/Resources/desktop"
cp desktop/* "$APP/Contents/Resources/desktop/"
cp ScreenLive.icns "$APP/Contents/Resources/ScreenLive.icns"
cp ../../LICENSE "$APP/Contents/Resources/LICENSE"
codesign --force --sign "${SCREENLIVE_SIGN_IDENTITY:--}" "$APP"
codesign --verify --strict "$APP"
ditto -c -k --keepParent "$APP" "$OUT/ScreenLive-macOS-$ARCH.zip"
