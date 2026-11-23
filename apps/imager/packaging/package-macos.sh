#!/usr/bin/env bash
# Package a built macOS imager as a person receives it: "Tilo Imager.app"
# holding the binary, the app's zip, and the DMG that holds the app and the
# /Applications link, each with a SHA-256 sidecar. It signs ad hoc, or with the
# Developer ID TILO_MAC_SIGNING_IDENTITY names, and notarizes and staples the
# app and the DMG when notary credentials are present beside the identity.
# Runs on macOS, on a binary built for the architecture it packages.
#
#   package-macos.sh <x86_64|aarch64> <imager-binary> <out-dir>
#
# With TILO_REQUIRE_SIGNING=1 a Developer ID identity, notary credentials and
# the expected team and certificate fingerprint are required, so a release
# build can never come out ad hoc signed.
set -euo pipefail

arch="${1:?architecture: x86_64 or aarch64}"
binary="${2:?the imager binary to package}"
out="${3:?output directory}"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"

[ "$(uname -s)" = Darwin ] \
  || { printf 'package-macos.sh runs on macOS only\n' >&2; exit 2; }
[ -f "$binary" ] || { printf '%s is not a file\n' "$binary" >&2; exit 2; }
case "$arch" in
  x86_64) wanted_arch=x86_64 ;;
  aarch64) wanted_arch=arm64 ;;
  *) printf 'unsupported architecture: %s\n' "$arch" >&2; exit 2 ;;
esac
# A package for one architecture must never carry the other's executable: a
# universal binary would run natively under Rosetta and prove nothing.
carried="$(lipo -archs "$binary")"
[ "$carried" = "$wanted_arch" ] \
  || { printf '%s is built for %s, not %s\n' "$binary" "$carried" "$wanted_arch" >&2; exit 1; }

version="${TILO_VERSION:-0.1.0}"
version="${version#imager-v}"
version="${version#v}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'version must be MAJOR.MINOR.PATCH, not %s\n' "$version" >&2; exit 2; }

identity="${TILO_MAC_SIGNING_IDENTITY:-}"
notary=0
if [ -n "${TILO_NOTARY_KEYCHAIN_PROFILE:-}" ] \
   || { [ -n "${TILO_APPLE_ID:-}" ] && [ -n "${TILO_APPLE_TEAM_ID:-}" ] \
        && [ -n "${TILO_APPLE_APP_PASSWORD:-}" ]; }; then
  notary=1
fi
require_signing="${TILO_REQUIRE_SIGNING:-0}"
if [ "$require_signing" = 1 ]; then
  [ -n "$identity" ] && [ "$notary" = 1 ] \
    || { printf 'macOS signing and notarization credentials are required for a release build\n' >&2; exit 1; }
  command -v codesign >/dev/null && command -v xcrun >/dev/null \
    || { printf 'codesign and xcrun are required\n' >&2; exit 1; }
fi
if [ "$notary" = 1 ] && [ -z "$identity" ]; then
  printf 'notarization needs a Developer ID signature: set TILO_MAC_SIGNING_IDENTITY\n' >&2
  exit 1
fi

mkdir -p "$out"
out="$(cd -- "$out" && pwd)"

app="$out/Tilo Imager.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/tilo-imager-bin"
cp "$repo/apps/imager/packaging/macos/tilo-imager" "$app/Contents/MacOS/tilo-imager"
chmod +x "$app/Contents/MacOS/tilo-imager" "$app/Contents/MacOS/tilo-imager-bin"
cp "$repo/apps/imager/packaging/macos/Info.plist" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" \
  -c "Add :CFBundleVersion string $version" "$app/Contents/Info.plist"
plutil -lint -s "$app/Contents/Info.plist"

# Inner code first, then the bundle, which seals the launcher beside it.
sign=(codesign --force --options runtime)
if [ -n "$identity" ]; then
  sign+=(--timestamp --sign "$identity")
else
  sign+=(--timestamp=none --sign -)
fi
"${sign[@]}" --identifier so.tilo.imager "$app/Contents/MacOS/tilo-imager-bin"
"${sign[@]}" "$app"
codesign --verify --deep --strict --verbose=2 "$app"

if [ -n "$identity" ]; then
  expected_team="${TILO_MAC_EXPECTED_TEAM_ID:-}"
  expected_cert="${TILO_MAC_EXPECTED_CERT_SHA256:-}"
  if [ "$require_signing" = 1 ]; then
    [[ "$expected_team" =~ ^[A-Z0-9]{10}$ ]] \
      || { printf 'TILO_MAC_EXPECTED_TEAM_ID is required for a release build\n' >&2; exit 1; }
    [[ "$expected_cert" =~ ^[A-F0-9]{64}$ ]] \
      || { printf 'TILO_MAC_EXPECTED_CERT_SHA256 is required for a release build\n' >&2; exit 1; }
  fi
  actual_team="$(codesign -dvv "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
  [ -z "$expected_team" ] || [ "$actual_team" = "$expected_team" ] \
    || { printf 'macOS Team ID does not match protected release policy\n' >&2; exit 1; }
  actual_cert="$(
    cert_tmp="$(mktemp -d)"
    trap 'rm -rf "$cert_tmp"' EXIT
    codesign -d --extract-certificates "$cert_tmp/cert" "$app"
    shasum -a 256 "$cert_tmp/cert0" | awk '{print toupper($1)}'
  )"
  [ -z "$expected_cert" ] || [ "$actual_cert" = "$expected_cert" ] \
    || { printf 'macOS signing certificate fingerprint does not match protected release policy\n' >&2; exit 1; }
fi

write_digest() {
  (cd "$(dirname -- "$1")" && shasum -a 256 "$(basename -- "$1")") >"$1.sha256"
}

notarize() {
  if [ -n "${TILO_NOTARY_KEYCHAIN_PROFILE:-}" ]; then
    xcrun notarytool submit "$1" --wait \
      --keychain-profile "$TILO_NOTARY_KEYCHAIN_PROFILE"
  else
    xcrun notarytool submit "$1" --wait \
      --apple-id "$TILO_APPLE_ID" --team-id "$TILO_APPLE_TEAM_ID" \
      --password "$TILO_APPLE_APP_PASSWORD"
  fi
}

artifact_base="tilo-imager-macos-$arch"
zip="$out/$artifact_base.zip"
dmg="$out/$artifact_base.dmg"
rm -f "$zip" "$zip.sha256" "$dmg" "$dmg.sha256" "$out/$artifact_base.bin.sha256"
# What the sign step compares against the build's own digest, so the signed
# DMG can only carry the executable the build check produced.
(cd "$app/Contents/MacOS" && shasum -a 256 tilo-imager-bin) >"$out/$artifact_base.bin.sha256"

ditto -c -k --keepParent "$app" "$zip"
if [ "$notary" = 1 ]; then
  notarize "$zip"
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  rm -f "$zip"
  ditto -c -k --keepParent "$app" "$zip"
fi
write_digest "$zip"

dmg_root="$(mktemp -d "${TMPDIR:-/tmp}/tilo-imager-dmg.XXXXXX")"
trap 'rm -rf -- "$dmg_root"' EXIT
ditto "$app" "$dmg_root/Tilo Imager.app"
ln -s /Applications "$dmg_root/Applications"
hdiutil create -quiet -volname "Tilo Imager" -srcfolder "$dmg_root" \
  -ov -format UDZO "$dmg"
if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
  codesign --verify --verbose=2 "$dmg"
  if [ "$notary" = 1 ]; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
  fi
fi
hdiutil verify -quiet "$dmg"
write_digest "$dmg"

printf 'packaged %s %s\n' "$app" "$version"
printf '  %s\n' "$zip" "$dmg" "$out/$artifact_base.bin.sha256"
