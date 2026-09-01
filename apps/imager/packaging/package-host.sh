#!/usr/bin/env bash
# Build a self-contained host bundle on Linux or macOS. Run on the target OS;
# this deliberately does not cross-compile platform disk APIs.
set -euo pipefail

out="${1:-dist/qos-imager}"
host="$(uname -s)"
require_signing="${QOS_REQUIRE_SIGNING:-0}"
require_native_packages="${QOS_REQUIRE_NATIVE_PACKAGES:-0}"
machine="$(uname -m)"
case "$machine" in
  x86_64|amd64) package_arch="x86_64" ;;
  aarch64|arm64) package_arch="aarch64" ;;
  *) printf 'unsupported architecture: %s\n' "$machine" >&2; exit 2 ;;
esac
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
    artifact_base="qos-imager-macos-$package_arch"
    ditto -c -k --keepParent "$app" "$out/$artifact_base.zip"
    if [ -n "${QOS_APPLE_ID:-}" ] && [ -n "${QOS_APPLE_TEAM_ID:-}" ] \
       && [ -n "${QOS_APPLE_APP_PASSWORD:-}" ]; then
      xcrun notarytool submit "$out/$artifact_base.zip" --wait \
        --apple-id "$QOS_APPLE_ID" --team-id "$QOS_APPLE_TEAM_ID" \
        --password "$QOS_APPLE_APP_PASSWORD"
      xcrun stapler staple "$app"
      rm -f "$out/$artifact_base.zip"
      ditto -c -k --keepParent "$app" "$out/$artifact_base.zip"
    elif [ "$require_signing" = 1 ]; then
      printf 'Apple notarization credentials are required for a release build\n' >&2
      exit 1
    fi
    write_digest "$out/$artifact_base.zip"
    if command -v hdiutil >/dev/null 2>&1; then
      dmg_root="$(mktemp -d)"
      cp -R "$app" "$dmg_root/"
      ln -s /Applications "$dmg_root/Applications"
      hdiutil create -quiet -volname "qOS Imager" -srcfolder "$dmg_root" \
        -ov -format UDZO "$out/$artifact_base.dmg"
      rm -rf "$dmg_root"
      write_digest "$out/$artifact_base.dmg"
    elif [ "$require_native_packages" = 1 ]; then
      printf 'hdiutil is required for the release DMG\n' >&2
      exit 1
    fi
    ;;
  Linux)
    artifact_base="qos-imager-linux-$package_arch"
    bundle="$out/$artifact_base"
    mkdir -p "$bundle"
    cp target/release/imager "$bundle/qos-imager-bin"
    cp apps/imager/packaging/linux/qos-imager "$bundle/qos-imager"
    chmod +x "$bundle/qos-imager" "$bundle/qos-imager-bin"
    cp apps/imager/packaging/linux/qos-imager.desktop "$bundle/"
    cp docs/installer-walkthrough.md "$bundle/README.md"
    tar -C "$out" -czf "$out/$artifact_base.tar.gz" "$artifact_base"
    write_digest "$out/$artifact_base.tar.gz"
    if [ -n "${QOS_LINUX_SIGNING_KEY:-}" ]; then
      gpg --batch --yes --local-user "$QOS_LINUX_SIGNING_KEY" \
        --armor --detach-sign "$out/$artifact_base.tar.gz"
    elif [ "$require_signing" = 1 ]; then
      printf 'QOS_LINUX_SIGNING_KEY is required for a release build\n' >&2
      exit 1
    fi

    appdir="$out/qOS_Imager.AppDir"
    mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications"
    cp target/release/imager "$appdir/usr/bin/qos-imager-bin"
    cp apps/imager/packaging/linux/qos-imager "$appdir/usr/bin/qos-imager"
    cp apps/imager/packaging/linux/qos-imager.desktop \
      "$appdir/usr/share/applications/org.qos.Imager.desktop"
    cp apps/imager/packaging/linux/AppRun "$appdir/AppRun"
    cp apps/imager/packaging/linux/qos-imager.svg "$appdir/qos-imager.svg"
    cp apps/imager/packaging/linux/qos-imager.svg "$appdir/.DirIcon"
    chmod +x "$appdir/AppRun" "$appdir/usr/bin/qos-imager" "$appdir/usr/bin/qos-imager-bin"
    if command -v linuxdeploy >/dev/null 2>&1 && command -v appimagetool >/dev/null 2>&1; then
      linuxdeploy --appdir "$appdir" >/dev/null
      ARCH="$package_arch" appimagetool "$appdir" "$out/$artifact_base.AppImage" >/dev/null
      chmod +x "$out/$artifact_base.AppImage"
      write_digest "$out/$artifact_base.AppImage"
    elif [ "$require_native_packages" = 1 ]; then
      printf 'linuxdeploy and appimagetool are required for the release AppImage\n' >&2
      exit 1
    fi

    if command -v flatpak-builder >/dev/null 2>&1; then
      flatpak_dir="$out/flatpak-build"
      flatpak_repo="$out/flatpak-repo"
      rm -rf "$flatpak_dir" "$flatpak_repo"
      flatpak-builder --force-clean --repo="$flatpak_repo" "$flatpak_dir" \
        apps/imager/packaging/linux/org.qos.Imager.yml
      flatpak build-bundle "$flatpak_repo" "$out/$artifact_base.flatpak" \
        org.qos.Imager stable
      write_digest "$out/$artifact_base.flatpak"
    elif [ "$require_native_packages" = 1 ]; then
      printf 'flatpak-builder is required for the release Flatpak bundle\n' >&2
      exit 1
    fi
    ;;
  *)
    printf 'unsupported host: %s\n' "$(uname -s)" >&2
    exit 2
    ;;
esac
