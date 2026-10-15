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
# Where Cargo puts its builds: the checkout's target/ unless CARGO_TARGET_DIR says otherwise.
cargo_target="${CARGO_TARGET_DIR:-target}"
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

# Self-update (dist/macos/README.md, "Updates"): the feed URL and public key go
# into Info.plist, sealed by the bundle signature; without both the bundle
# updates only through its installer. With TILO_RUNTIME_UPDATE_KEY_FILE the
# build also writes the signed feed for this release.
update_feed="${TILO_RUNTIME_UPDATE_FEED:-}"
update_public_key="${TILO_RUNTIME_UPDATE_PUBLIC_KEY:-}"
update_key_file="${TILO_RUNTIME_UPDATE_KEY_FILE:-}"
if [ -n "$update_feed$update_public_key" ]; then
  [[ "$update_feed" =~ ^https://[^[:space:]]+$ ]] \
    || { printf 'TILO_RUNTIME_UPDATE_FEED must be an https URL\n' >&2; exit 2; }
  [[ "$update_public_key" =~ ^[0-9a-f]{64}$ ]] \
    || { printf 'TILO_RUNTIME_UPDATE_PUBLIC_KEY must be 64 lowercase hex digits\n' >&2; exit 2; }
fi
if [ -n "$update_key_file" ] && [ -z "$update_feed" ]; then
  printf 'signing an update feed needs TILO_RUNTIME_UPDATE_FEED and its public key\n' >&2
  exit 2
fi

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
  -p runtime-update --bin tilo-runtime-update \
  "${targets[@]/#/--target=}"

app="$out/Tilo.app"
contents="$app/Contents"
rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Library/LaunchAgents"
lipo -create -output "$contents/MacOS/Tilo" \
  "$cargo_target"/aarch64-apple-darwin/release/store \
  "$cargo_target"/x86_64-apple-darwin/release/store
for binary in tilo-runtime tilo-runtime-update; do
  lipo -create -output "$contents/MacOS/$binary" \
    "$cargo_target/aarch64-apple-darwin/release/$binary" \
    "$cargo_target/x86_64-apple-darwin/release/$binary"
done
for binary in "$contents/MacOS/Tilo" "$contents/MacOS/tilo-runtime" \
              "$contents/MacOS/tilo-runtime-update"; do
  [ "$(lipo -archs "$binary" | tr ' ' '\n' | sort | xargs)" = "arm64 x86_64" ] \
    || { printf '%s is not universal\n' "$binary" >&2; exit 1; }
done
cp "$here/Info.plist" "$contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" \
  -c "Set :CFBundleVersion $version" "$contents/Info.plist"
if [ -n "$update_feed" ]; then
  /usr/libexec/PlistBuddy -c "Add :TiloUpdateFeedURL string $update_feed" \
    -c "Add :TiloUpdatePublicKey string $update_public_key" "$contents/Info.plist"
fi
for agent in so.tilo.runtime so.tilo.runtime.update; do
  cp "$here/$agent.plist" "$contents/Library/LaunchAgents/$agent.plist"
done
plutil -lint -s "$contents/Info.plist" "$contents/Library/LaunchAgents/"*.plist \
  "$here/Tilo.entitlements" "$here/Runtime.entitlements"

# Inner code first, then the bundle, which seals the main executable, the
# agent plist and the already-signed runtime. No --deep: it would re-sign the
# runtime with the bundle's identifier and options.
sign=(codesign --force --options runtime)
if [ -n "$identity" ]; then
  sign+=(--timestamp --sign "$identity")
else
  sign+=(--timestamp=none --sign -)
fi
"${sign[@]}" --entitlements "$here/Runtime.entitlements" --identifier so.tilo.runtime.daemon \
  "$contents/MacOS/tilo-runtime"
"${sign[@]}" --entitlements "$here/Tilo.entitlements" --identifier so.tilo.runtime.update \
  "$contents/MacOS/tilo-runtime-update"
"${sign[@]}" --entitlements "$here/Tilo.entitlements" "$app"
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

# The feed names the final (stapled) zip, and its signature covers these exact
# bytes, so it is written compactly and never reformatted.
feed="$out/tilo-macos-update.json"
rm -f "$feed" "$feed.sig"
if [ -n "$update_key_file" ]; then
  printf '{"version":"%s","url":"%s","sha256":"%s"}' "$version" \
    "$release_page/releases/download/runtime-v$version/tilo-macos-universal.zip" \
    "$(awk '{print $1}' "$artifact.zip.sha256")" >"$feed"
  "$cargo_target"/aarch64-apple-darwin/release/tilo-runtime-update sign --key "$update_key_file" \
    "$feed" >"$feed.sig"
fi

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
if [ -n "$update_feed" ]; then
  # Homebrew leaves a self-updating app to itself instead of reinstalling it.
  sed -i '' '/^  homepage /a\
  auto_updates true
' "$out/tilo.rb"
fi
if grep -En '@[A-Z0-9_]+@' "$out/tilo.rb"; then
  printf 'unresolved Homebrew template placeholder\n' >&2
  exit 1
fi
ruby -c "$out/tilo.rb" >/dev/null

printf 'built %s %s\n' "$app" "$version"
printf '  %s\n' "$artifact.zip" "$artifact.dmg" "$out/tilo.rb"
[ ! -f "$feed" ] || printf '  %s\n' "$feed" "$feed.sig"
