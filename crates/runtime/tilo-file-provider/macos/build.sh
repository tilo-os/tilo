#!/usr/bin/env bash
# Build the Tilo File Provider extension and embed it, with its domain tool,
# into a Tilo.app: the one dist/macos/build-app.sh built when APP is given,
# otherwise a development bundle holding only these two. Signs ad hoc unless
# TILO_MAC_SIGNING_IDENTITY names a Developer ID. Host architecture only.
#
#   crates/runtime/tilo-file-provider/macos/build.sh [APP]
#   APP/Contents/MacOS/tilo-files add      # then enable it: see README.md
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
cd "$repo"
[ "$(uname -s)" = Darwin ] || { printf 'build.sh runs on macOS only\n' >&2; exit 2; }
here=crates/runtime/tilo-file-provider/macos

app="${1:-target/tilo-file-provider/Tilo.app}"
contents="$app/Contents"
if [ ! -f "$contents/Info.plist" ]; then
  # Tilo.app's identity, so the extension is found where it will ship.
  mkdir -p "$contents/MacOS"
  plist="$contents/Info.plist"
  /usr/libexec/PlistBuddy -c "Add :CFBundleIdentifier string so.tilo.runtime" \
    -c "Add :CFBundleName string Tilo" -c "Add :CFBundleExecutable string tilo-files" \
    -c "Add :CFBundlePackageType string APPL" -c "Add :CFBundleShortVersionString string 0.0.0" \
    -c "Add :CFBundleVersion string 0.0.0" -c "Add :LSMinimumSystemVersion string 13.0" "$plist"
fi
version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$contents/Info.plist")"

cargo build --locked --release -p tilo-file-provider

appex="$contents/PlugIns/TiloFiles.appex"
rm -rf "$appex"
mkdir -p "$appex/Contents/MacOS"
cp "$here/Info.plist" "$appex/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" \
  -c "Set :CFBundleVersion $version" "$appex/Contents/Info.plist"
plutil -lint -s "$appex/Contents/Info.plist" "$here/TiloFiles.entitlements"

swiftc -O -parse-as-library -module-name TiloFiles \
  -import-objc-header "$here/TiloFileProvider.h" \
  "$here/FileProviderExtension.swift" \
  -L target/release -ltilo_file_provider \
  -framework FileProvider -Xlinker -e -Xlinker _NSExtensionMain \
  -o "$appex/Contents/MacOS/TiloFiles"
swiftc -O "$here/tilo-files.swift" -framework FileProvider -o "$contents/MacOS/tilo-files"

# Inner code first, then the bundle, which seals them; never --deep.
sign=(codesign --force --options runtime)
if [ -n "${TILO_MAC_SIGNING_IDENTITY:-}" ]; then
  sign+=(--timestamp --sign "$TILO_MAC_SIGNING_IDENTITY")
else
  sign+=(--timestamp=none --sign -)
fi
"${sign[@]}" --entitlements "$here/TiloFiles.entitlements" "$appex"
"${sign[@]}" --identifier so.tilo.runtime.files-tool "$contents/MacOS/tilo-files"
"${sign[@]}" --preserve-metadata=entitlements "$app"
codesign --verify --deep --strict --verbose=2 "$app"

printf 'built %s\n' "$appex"
