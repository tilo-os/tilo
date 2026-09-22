#!/bin/bash
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
    echo "This bundle script requires macOS." >&2
    exit 1
fi

shell_repo="$(cd "$(dirname "$0")/.." && pwd)"
shell_bundle="$shell_repo/target/macos/Tilo.app"
cd "$shell_repo"
cargo build --locked -p shell --bin shell --no-default-features
mkdir -p "$shell_bundle/Contents/MacOS"
cp "$shell_repo/target/debug/shell" "$shell_bundle/Contents/MacOS/Tilo"
cat > "$shell_bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>Tilo</string>
  <key>CFBundleIdentifier</key><string>so.tilo.shell</string>
  <key>CFBundleName</key><string>Tilo</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$shell_bundle"
printf 'Built %s\n' "$shell_bundle"
