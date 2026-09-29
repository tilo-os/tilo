#!/usr/bin/env bash
# Build the distributed Tilo.app for macOS: a universal (arm64 + x86_64)
# bundle whose visible app is the Store and whose sibling executable is the
# per-user runtime, then its zip, DMG and Homebrew cask. Signs ad hoc unless
# TILO_MAC_SIGNING_IDENTITY names a Developer ID; notarizes and staples when
# notary credentials are also present. See dist/macos/README.md.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo"
[ "$(uname -s)" = Darwin ] \
  || { printf 'build-app.sh runs on macOS only\n' >&2; exit 2; }

out="${1:-target/dist-macos}"
here=dist/macos
version="${TILO_VERSION:-$(<dist/VERSION)}"
version="${version#runtime-v}"
version="${version#v}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'version must be MAJOR.MINOR.PATCH, got %s\n' "$version" >&2; exit 2; }
release_repository="${TILO_RELEASE_REPOSITORY:-tilo-os/releases}"
release_server="${TILO_RELEASE_SERVER:-https://github.com}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[[ "$release_server" =~ ^https://[^/]+$ ]] \
  || { printf 'release server must be an HTTPS origin\n' >&2; exit 2; }
release_page="${release_server}/${release_repository}"

require_signing="${TILO_REQUIRE_SIGNING:-0}"
identity="${TILO_MAC_SIGNING_IDENTITY:-}"
notary=0
if [ -n "${TILO_NOTARY_KEYCHAIN_PROFILE:-}" ] \
   || { [ -n "${TILO_APPLE_ID:-}" ] && [ -n "${TILO_APPLE_TEAM_ID:-}" ] \
        && [ -n "${TILO_APPLE_APP_PASSWORD:-}" ]; }; then
  notary=1
fi
if [ "$require_signing" = 1 ]; then
  [ -n "$identity" ] && [ "$notary" = 1 ] \
    || { printf 'macOS signing and notarization credentials are required\n' >&2; exit 1; }
fi
if [ "$notary" = 1 ] && [ -z "$identity" ]; then
  printf 'notarization needs a Developer ID signature; set TILO_MAC_SIGNING_IDENTITY\n' >&2
  exit 1
fi

targets=(aarch64-apple-darwin x86_64-apple-darwin)
installed_targets="$(rustup target list --installed)"
for target in "${targets[@]}"; do
  grep -qx "$target" <<<"$installed_targets" \
    || { printf 'missing Rust target %s; run: rustup target add %s\n' "$target" "$target" >&2; exit 1; }
done
cargo build --locked --release \
  -p tilo-runtime --bin tilo-runtime -p store --bin store \
  "${targets[@]/#/--target=}"

app="$out/Tilo.app"
contents="$app/Contents"
rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Library/LaunchAgents"
lipo -create -output "$contents/MacOS/Tilo" \
  target/aarch64-apple-darwin/release/store \
  target/x86_64-apple-darwin/release/store
lipo -create -output "$contents/MacOS/tilo-runtime" \
  target/aarch64-apple-darwin/release/tilo-runtime \
  target/x86_64-apple-darwin/release/tilo-runtime
for binary in "$contents/MacOS/Tilo" "$contents/MacOS/tilo-runtime"; do
  [ "$(lipo -archs "$binary" | tr ' ' '\n' | sort | xargs)" = "arm64 x86_64" ] \
    || { printf '%s is not universal\n' "$binary" >&2; exit 1; }
done
cp "$here/Info.plist" "$contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" \
  -c "Set :CFBundleVersion $version" "$contents/Info.plist"
cp "$here/so.tilo.runtime.plist" "$contents/Library/LaunchAgents/so.tilo.runtime.plist"
plutil -lint -s "$contents/Info.plist" \
  "$contents/Library/LaunchAgents/so.tilo.runtime.plist" "$here/Tilo.entitlements"

# Inner code first, then the bundle, which seals the main executable, the
# agent plist and the already-signed runtime. No --deep: it would re-sign the
# runtime with the bundle's identifier and options.
sign=(codesign --force --options runtime --entitlements "$here/Tilo.entitlements")
if [ -n "$identity" ]; then
  sign+=(--timestamp --sign "$identity")
else
  sign+=(--timestamp=none --sign -)
fi
"${sign[@]}" --identifier so.tilo.runtime.daemon "$contents/MacOS/tilo-runtime"
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

write_digest() {
  (cd "$(dirname "$1")" && shasum -a 256 "$(basename "$1")") >"$1.sha256"
}

artifact="$out/tilo-macos-universal"
rm -f "$artifact.zip" "$artifact.dmg"
ditto -c -k --keepParent "$app" "$artifact.zip"
if [ "$notary" = 1 ]; then
  notarize "$artifact.zip"
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  rm -f "$artifact.zip"
  ditto -c -k --keepParent "$app" "$artifact.zip"
fi
write_digest "$artifact.zip"

dmg_root="$(mktemp -d "${TMPDIR:-/tmp}/tilo-dmg.XXXXXX")"
trap 'rm -rf -- "$dmg_root"' EXIT
ditto "$app" "$dmg_root/Tilo.app"
ln -s /Applications "$dmg_root/Applications"
hdiutil create -quiet -volname Tilo -srcfolder "$dmg_root" \
  -ov -format UDZO "$artifact.dmg"
if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$artifact.dmg"
  codesign --verify --verbose=2 "$artifact.dmg"
  if [ "$notary" = 1 ]; then
    notarize "$artifact.dmg"
    xcrun stapler staple "$artifact.dmg"
    xcrun stapler validate "$artifact.dmg"
  fi
fi
hdiutil verify -quiet "$artifact.dmg"
write_digest "$artifact.dmg"

dmg_sha256="$(awk '{print $1}' "$artifact.dmg.sha256")"
sed -e "s|@VERSION@|$version|g" \
    -e "s|@RELEASE_PAGE@|$release_page|g" \
    -e "s|@MACOS_UNIVERSAL_SHA256@|$dmg_sha256|g" \
    "$here/homebrew/tilo-cask.rb.in" >"$out/tilo.rb"
if grep -En '@[A-Z0-9_]+@' "$out/tilo.rb"; then
  printf 'unresolved Homebrew template placeholder\n' >&2
  exit 1
fi
ruby -c "$out/tilo.rb" >/dev/null

printf 'built %s %s\n' "$app" "$version"
printf '  %s\n' "$artifact.zip" "$artifact.dmg" "$out/tilo.rb"
