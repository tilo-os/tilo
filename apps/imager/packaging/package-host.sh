#!/usr/bin/env bash
# Build a self-contained host bundle on Linux or macOS. Run on the target OS;
# this deliberately does not cross-compile platform disk APIs.
set -euo pipefail

out="${1:-dist/qos-imager}"
host="$(uname -s)"
require_signing="${QOS_REQUIRE_SIGNING:-0}"
if [ "$require_signing" = 1 ]; then
  case "$host" in
    Darwin)
      [ -n "${QOS_MAC_SIGNING_IDENTITY:-}" ] \
        && [ -n "${QOS_APPLE_ID:-}" ] \
        && [ -n "${QOS_APPLE_TEAM_ID:-}" ] \
        && [ -n "${QOS_APPLE_APP_PASSWORD:-}" ] \
        || { printf 'macOS signing and notarization credentials are required\n' >&2; exit 1; }
      command -v codesign >/dev/null && command -v xcrun >/dev/null \
        || { printf 'codesign and xcrun are required\n' >&2; exit 1; }
      ;;
    Linux)
      [ -n "${QOS_LINUX_SIGNING_KEY:-}" ] \
        || { printf 'QOS_LINUX_SIGNING_KEY is required\n' >&2; exit 1; }
      command -v gpg >/dev/null \
        || { printf 'gpg is required for a signed Linux release\n' >&2; exit 1; }
      ;;
  esac
fi
cargo build --locked --release -p imager
mkdir -p "$out"

write_digest() {
  artifact="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$(dirname "$artifact")" && sha256sum "$(basename "$artifact")") >"$artifact.sha256"
  else
    (cd "$(dirname "$artifact")" && shasum -a 256 "$(basename "$artifact")") >"$artifact.sha256"
  fi
}

case "$host" in
  Darwin)
    app="$out/qOS Imager.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp target/release/imager "$app/Contents/MacOS/qos-imager-bin"
    cp apps/imager/packaging/macos/qos-imager "$app/Contents/MacOS/qos-imager"
    chmod +x "$app/Contents/MacOS/qos-imager" "$app/Contents/MacOS/qos-imager-bin"
    cp apps/imager/packaging/macos/Info.plist "$app/Contents/Info.plist"
    if [ -n "${QOS_MAC_SIGNING_IDENTITY:-}" ]; then
      codesign --force --options runtime --timestamp \
        --sign "$QOS_MAC_SIGNING_IDENTITY" "$app/Contents/MacOS/qos-imager-bin"
      codesign --force --options runtime --timestamp \
        --sign "$QOS_MAC_SIGNING_IDENTITY" "$app"
      codesign --verify --deep --strict --verbose=2 "$app"
    elif [ "$require_signing" = 1 ]; then
      printf 'QOS_MAC_SIGNING_IDENTITY is required for a release build\n' >&2
      exit 1
    fi
    ditto -c -k --keepParent "$app" "$out/qos-imager-macos.zip"
    if [ -n "${QOS_APPLE_ID:-}" ] && [ -n "${QOS_APPLE_TEAM_ID:-}" ] \
       && [ -n "${QOS_APPLE_APP_PASSWORD:-}" ]; then
      xcrun notarytool submit "$out/qos-imager-macos.zip" --wait \
        --apple-id "$QOS_APPLE_ID" --team-id "$QOS_APPLE_TEAM_ID" \
        --password "$QOS_APPLE_APP_PASSWORD"
      xcrun stapler staple "$app"
      rm -f "$out/qos-imager-macos.zip"
      ditto -c -k --keepParent "$app" "$out/qos-imager-macos.zip"
    elif [ "$require_signing" = 1 ]; then
      printf 'Apple notarization credentials are required for a release build\n' >&2
      exit 1
    fi
    write_digest "$out/qos-imager-macos.zip"
    ;;
  Linux)
    bundle="$out/qos-imager-linux"
    mkdir -p "$bundle"
    cp target/release/imager "$bundle/qos-imager-bin"
    cp apps/imager/packaging/linux/qos-imager "$bundle/qos-imager"
    chmod +x "$bundle/qos-imager" "$bundle/qos-imager-bin"
    cp apps/imager/packaging/linux/qos-imager.desktop "$bundle/"
    cp docs/installer-walkthrough.md "$bundle/README.md"
    tar -C "$out" -czf "$out/qos-imager-linux.tar.gz" qos-imager-linux
    write_digest "$out/qos-imager-linux.tar.gz"
    if [ -n "${QOS_LINUX_SIGNING_KEY:-}" ]; then
      gpg --batch --yes --local-user "$QOS_LINUX_SIGNING_KEY" \
        --armor --detach-sign "$out/qos-imager-linux.tar.gz"
    elif [ "$require_signing" = 1 ]; then
      printf 'QOS_LINUX_SIGNING_KEY is required for a release build\n' >&2
      exit 1
    fi
    ;;
  *)
    printf 'unsupported host: %s\n' "$(uname -s)" >&2
    exit 2
    ;;
esac
