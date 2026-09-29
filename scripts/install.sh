#!/bin/zsh
# Build, sign, install to /Applications and launch.
#
# A stable code signature is what lets macOS remember the Screen Recording permission across
# rebuilds. Set FLICK_SIGN_ID (see `security find-identity -v -p codesigning`) in the environment
# or in scripts/.signing (git-ignored). Without it the app is ad-hoc signed and macOS will ask for
# permission again after every rebuild.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -f scripts/.signing ]] && source scripts/.signing
IDENTITY="${FLICK_SIGN_ID:--}"
[[ "$IDENTITY" == "-" ]] && print -u2 "warning: FLICK_SIGN_ID not set, using ad-hoc signature"

npx tauri build
APP=src-tauri/target/release/bundle/macos/Flick.app
codesign --force --deep --options runtime --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict "$APP"

pkill -x flick || true
rm -rf /Applications/Flick.app
cp -R "$APP" /Applications/
open /Applications/Flick.app
