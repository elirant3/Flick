#!/bin/zsh
# Build, sign with a stable identity (so macOS keeps the Screen Recording grant across rebuilds), install, launch.
set -e
cd "$(dirname "$0")/.."
IDENTITY="${FLICK_SIGN_ID:--}"
npx tauri build
APP=src-tauri/target/release/bundle/macos/Flick.app
codesign --force --deep --options runtime --sign "$IDENTITY" "$APP"
pkill -x flick || true
rm -rf /Applications/Flick.app
cp -R "$APP" /Applications/
open /Applications/Flick.app
