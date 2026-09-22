#!/bin/bash
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
    echo "This bundle script requires macOS." >&2
    exit 1
fi

scratchpad_repo="$(cd "$(dirname "$0")/../.." && pwd)"
scratchpad_bundle="$scratchpad_repo/target/macos/Scratchpad.app"
cd "$scratchpad_repo"
cargo build --locked -p scratchpad
mkdir -p "$scratchpad_bundle/Contents/MacOS"
cp "$scratchpad_repo/target/debug/scratchpad" "$scratchpad_bundle/Contents/MacOS/Scratchpad"
cat > "$scratchpad_bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>Scratchpad</string>
  <key>CFBundleIdentifier</key><string>so.tilo.scratchpad</string>
  <key>CFBundleName</key><string>Scratchpad</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$scratchpad_bundle"
printf 'Built %s\n' "$scratchpad_bundle"
