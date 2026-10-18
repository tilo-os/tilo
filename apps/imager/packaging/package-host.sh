#!/usr/bin/env bash
# Build a self-contained host bundle on Linux or macOS. Run on the target OS;
# this deliberately does not cross-compile platform disk APIs.
set -euo pipefail

out="${1:-dist/tilo-imager}"
host="$(uname -s)"
require_signing="${TILO_REQUIRE_SIGNING:-0}"
require_native_packages="${TILO_REQUIRE_NATIVE_PACKAGES:-0}"
version="${TILO_VERSION:-0.1.0}"
version="${version#imager-v}"
version="${version#v}"
release_repository="${TILO_RELEASE_REPOSITORY:-tilo-os/releases}"
release_server="${TILO_RELEASE_SERVER:-https://github.com}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[[ "$release_server" =~ ^https://[^/]+$ ]] \
  || { printf 'release server must be an HTTPS origin\n' >&2; exit 2; }
release_page="${release_server}/${release_repository}"
machine="$(uname -m)"
case "$machine" in
  x86_64|amd64) package_arch="x86_64" ;;
  aarch64|arm64) package_arch="aarch64" ;;
  *) printf 'unsupported architecture: %s\n' "$machine" >&2; exit 2 ;;
esac
if [ "$require_signing" = 1 ]; then
  case "$host" in
    Darwin)
      [ -n "${TILO_MAC_SIGNING_IDENTITY:-}" ] \
        && { [ -n "${TILO_NOTARY_KEYCHAIN_PROFILE:-}" ] \
             || { [ -n "${TILO_APPLE_ID:-}" ] \
                  && [ -n "${TILO_APPLE_TEAM_ID:-}" ] \
                  && [ -n "${TILO_APPLE_APP_PASSWORD:-}" ]; }; } \
        || { printf 'macOS signing and notarization credentials are required\n' >&2; exit 1; }
      command -v codesign >/dev/null && command -v xcrun >/dev/null \
        || { printf 'codesign and xcrun are required\n' >&2; exit 1; }
      ;;
    Linux)
      { [ -n "${TILO_LINUX_SIGNER:-}" ] && [ -n "${TILO_LINUX_VERIFY_KEY:-}" ]; } \
        || [ -n "${TILO_LINUX_SIGNING_KEY:-}" ] \
        || { printf 'TILO_LINUX_SIGNER + TILO_LINUX_VERIFY_KEY or TILO_LINUX_SIGNING_KEY are required\n' >&2; exit 1; }
      command -v gpg >/dev/null \
        || { printf 'gpg is required for a signed Linux release\n' >&2; exit 1; }
      ;;
  esac
fi
if [ "$host" = Linux ] && [ "$require_signing" = 1 ]; then export TILO_REQUIRE_CAPTURE=1; fi
source "$(dirname "${BASH_SOURCE[0]}")/build-capture-host.sh" --features host-media-writer
mkdir -p "$out"

write_digest() {
  artifact="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$(dirname "$artifact")" && sha256sum "$(basename "$artifact")") >"$artifact.sha256"
  else
    (cd "$(dirname "$artifact")" && shasum -a 256 "$(basename "$artifact")") >"$artifact.sha256"
  fi
}

sign_linux_artifact() {
  artifact="$1"
  if [ -n "${TILO_LINUX_SIGNER:-}" ]; then
    "$TILO_LINUX_SIGNER" "$artifact" "$artifact.asc"
    [ -s "$artifact.asc" ] \
      || { printf 'external Linux signer did not create %s.asc\n' "$artifact" >&2; exit 1; }
    signature_identity="$("${linux_release_gpg[@]}" --status-fd 1 \
      --verify "$artifact.asc" "$artifact" 2>/dev/null \
      | awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" { print $3 " " $NF; exit }')"
    read -r signing_fingerprint primary_fingerprint <<<"$signature_identity"
    if [ "$signing_fingerprint" != "$linux_release_fingerprint" ] \
       && [ "$primary_fingerprint" != "$linux_release_fingerprint" ]; then
      printf 'Linux signature was not made by the expected release key %s\n' \
        "$linux_release_fingerprint" >&2
      exit 1
    fi
  elif [ -n "${TILO_LINUX_SIGNING_KEY:-}" ]; then
    gpg --batch --yes --local-user "$TILO_LINUX_SIGNING_KEY" \
      --armor --detach-sign "$artifact"
    signature_identity="$("${linux_release_gpg[@]}" --status-fd 1 \
      --verify "$artifact.asc" "$artifact" 2>/dev/null \
      | awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" { print $3 " " $NF; exit }')"
    read -r signing_fingerprint primary_fingerprint <<<"$signature_identity"
    if [ "$signing_fingerprint" != "$linux_release_fingerprint" ] \
       && [ "$primary_fingerprint" != "$linux_release_fingerprint" ]; then
      printf 'Linux signature was not made by the expected release key %s\n' \
        "$linux_release_fingerprint" >&2
      exit 1
    fi
  elif [ "$require_signing" = 1 ]; then
    printf 'Linux signing credentials are required for a release build\n' >&2
    exit 1
  fi
}

prepare_linux_release_identity() {
  local selector expected key_file fingerprint_file verify_home
  local -a fingerprints
  selector="${TILO_LINUX_VERIFY_KEY:-${TILO_LINUX_SIGNING_KEY:-}}"
  [ -n "$selector" ] \
    || { printf 'no Linux release-key selector was configured\n' >&2; exit 1; }

  linux_release_gpg=(gpg --batch)
  if [ -n "${TILO_LINUX_VERIFY_KEY:-}" ]; then
    [ -f "$TILO_LINUX_VERIFY_KEY" ] && [ ! -L "$TILO_LINUX_VERIFY_KEY" ] \
      || { printf 'TILO_LINUX_VERIFY_KEY must be a regular public-key file\n' >&2; exit 1; }
    mapfile -t fingerprints < <(
      gpg --batch --with-colons --show-keys --fingerprint \
        "$TILO_LINUX_VERIFY_KEY" 2>/dev/null \
        | awk -F: '$1 == "pub" { want = 1; next }
                     want && $1 == "fpr" { print toupper($10); want = 0 }'
    )
  else
    mapfile -t fingerprints < <(
      gpg --batch --with-colons --fingerprint "$selector" 2>/dev/null \
        | awk -F: '$1 == "pub" { want = 1; next }
                     want && $1 == "fpr" { print toupper($10); want = 0 }'
    )
  fi
  [ "${#fingerprints[@]}" -eq 1 ] \
    || { printf 'Linux release-key selector must resolve to exactly one primary key\n' >&2; exit 1; }
  linux_release_fingerprint="${fingerprints[0]}"
  [[ "$linux_release_fingerprint" =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] \
    || { printf 'Linux release key has an invalid OpenPGP fingerprint\n' >&2; exit 1; }

  expected="${TILO_LINUX_EXPECTED_FINGERPRINT:-}"
  expected="${expected^^}"
  expected="${expected//[[:space:]]/}"
  [[ "$expected" =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] \
    || { printf 'TILO_LINUX_EXPECTED_FINGERPRINT must be a full OpenPGP fingerprint\n' >&2; exit 1; }
  [ "$linux_release_fingerprint" = "$expected" ] \
    || { printf 'Linux release key fingerprint does not match protected release policy\n' >&2; exit 1; }

  if [ -n "${TILO_LINUX_VERIFY_KEY:-}" ]; then
    verify_home="$(mktemp -d "${TMPDIR:-/tmp}/tilo-imager-verify.XXXXXX")"
    chmod 0700 "$verify_home"
    linux_release_verify_home="$verify_home"
    trap 'gpgconf --homedir "$linux_release_verify_home" --kill all >/dev/null 2>&1 || true; rm -rf -- "$linux_release_verify_home"' EXIT
    gpg --batch --homedir "$verify_home" --import-options import-minimal \
      --import "$TILO_LINUX_VERIFY_KEY" >/dev/null 2>&1 \
      || { printf 'could not import the Linux release verification key\n' >&2; exit 1; }
    linux_release_gpg=(gpg --batch --homedir "$verify_home")
  fi

  key_file="$out/tilo-release-key.asc"
  fingerprint_file="$out/tilo-release-key.fingerprint"
  "${linux_release_gpg[@]}" --armor --export "$linux_release_fingerprint" >"$key_file"
  [ -s "$key_file" ] \
    || { printf 'could not export the Linux release public key\n' >&2; exit 1; }
  printf '%s\n' "$linux_release_fingerprint" >"$fingerprint_file"
  write_digest "$key_file"
  write_digest "$fingerprint_file"
}

case "$host" in
  Darwin)
    app="$out/Tilo Imager.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp target/release/imager "$app/Contents/MacOS/tilo-imager-bin"
    cp apps/imager/packaging/macos/tilo-imager "$app/Contents/MacOS/tilo-imager"
    chmod +x "$app/Contents/MacOS/tilo-imager" "$app/Contents/MacOS/tilo-imager-bin"
    cp apps/imager/packaging/macos/Info.plist "$app/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" \
      "$app/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Add :CFBundleVersion string $version" \
      "$app/Contents/Info.plist"
    if [ -n "${TILO_MAC_SIGNING_IDENTITY:-}" ]; then
      codesign --force --options runtime --timestamp \
        --sign "$TILO_MAC_SIGNING_IDENTITY" "$app/Contents/MacOS/tilo-imager-bin"
      codesign --force --options runtime --timestamp \
        --sign "$TILO_MAC_SIGNING_IDENTITY" "$app"
      codesign --verify --deep --strict --verbose=2 "$app"
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
    elif [ "$require_signing" = 1 ]; then
      printf 'TILO_MAC_SIGNING_IDENTITY is required for a release build\n' >&2
      exit 1
    fi
    artifact_base="tilo-imager-macos-$package_arch"
    ditto -c -k --keepParent "$app" "$out/$artifact_base.zip"
    if [ -n "${TILO_NOTARY_KEYCHAIN_PROFILE:-}" ]; then
      xcrun notarytool submit "$out/$artifact_base.zip" --wait \
        --keychain-profile "$TILO_NOTARY_KEYCHAIN_PROFILE"
      xcrun stapler staple "$app"
      rm -f "$out/$artifact_base.zip"
      ditto -c -k --keepParent "$app" "$out/$artifact_base.zip"
    elif [ -n "${TILO_APPLE_ID:-}" ] && [ -n "${TILO_APPLE_TEAM_ID:-}" ] \
         && [ -n "${TILO_APPLE_APP_PASSWORD:-}" ]; then
      xcrun notarytool submit "$out/$artifact_base.zip" --wait \
        --apple-id "$TILO_APPLE_ID" --team-id "$TILO_APPLE_TEAM_ID" \
        --password "$TILO_APPLE_APP_PASSWORD"
      xcrun stapler staple "$app"
      rm -f "$out/$artifact_base.zip"
      ditto -c -k --keepParent "$app" "$out/$artifact_base.zip"
    elif [ "$require_signing" = 1 ]; then
      printf 'Apple notarization credentials are required for a release build\n' >&2
      exit 1
    fi
    if [ -n "${TILO_NOTARY_KEYCHAIN_PROFILE:-}" ] \
       || { [ -n "${TILO_APPLE_ID:-}" ] && [ -n "${TILO_APPLE_TEAM_ID:-}" ] \
            && [ -n "${TILO_APPLE_APP_PASSWORD:-}" ]; }; then
      xcrun stapler validate "$app"
    fi
    write_digest "$out/$artifact_base.zip"
    if command -v hdiutil >/dev/null 2>&1; then
      dmg_root="$(mktemp -d)"
      cp -R "$app" "$dmg_root/"
      ln -s /Applications "$dmg_root/Applications"
      hdiutil create -quiet -volname "Tilo Imager" -srcfolder "$dmg_root" \
        -ov -format UDZO "$out/$artifact_base.dmg"
      rm -rf "$dmg_root"
      if [ -n "${TILO_MAC_SIGNING_IDENTITY:-}" ]; then
        codesign --force --timestamp --sign "$TILO_MAC_SIGNING_IDENTITY" \
          "$out/$artifact_base.dmg"
        codesign --verify --verbose=2 "$out/$artifact_base.dmg"
        if [ -n "${TILO_NOTARY_KEYCHAIN_PROFILE:-}" ]; then
          xcrun notarytool submit "$out/$artifact_base.dmg" --wait \
            --keychain-profile "$TILO_NOTARY_KEYCHAIN_PROFILE"
        else
          xcrun notarytool submit "$out/$artifact_base.dmg" --wait \
            --apple-id "$TILO_APPLE_ID" --team-id "$TILO_APPLE_TEAM_ID" \
            --password "$TILO_APPLE_APP_PASSWORD"
        fi
        xcrun stapler staple "$out/$artifact_base.dmg"
        xcrun stapler validate "$out/$artifact_base.dmg"
      fi
      write_digest "$out/$artifact_base.dmg"
    elif [ "$require_native_packages" = 1 ]; then
      printf 'hdiutil is required for the release DMG\n' >&2
      exit 1
    fi
    ;;
  Linux)
    if [ "$require_signing" = 1 ] || [ -n "${TILO_LINUX_SIGNER:-}" ] \
       || [ -n "${TILO_LINUX_SIGNING_KEY:-}" ]; then
      prepare_linux_release_identity
    fi
    artifact_base="tilo-imager-linux-$package_arch"
    bundle="$out/$artifact_base"
    mkdir -p "$bundle"
    cp target/release/imager "$bundle/tilo-imager-bin"
    install -m755 target/release/tilo-enroll "$bundle/tilo-enroll"
    cp apps/imager/packaging/linux/tilo-imager "$bundle/tilo-imager"
    chmod +x "$bundle/tilo-imager" "$bundle/tilo-imager-bin"
    cp apps/imager/packaging/linux/tilo-imager.desktop "$bundle/"
    cp docs/installer-walkthrough.md "$bundle/README.md"
    apps/imager/packaging/stage-capture-sidecar.sh target/release/imager "$bundle" "$bundle/tilo-imager-bin"
    tar -C "$out" -czf "$out/$artifact_base.tar.gz" "$artifact_base"
    write_digest "$out/$artifact_base.tar.gz"
    sign_linux_artifact "$out/$artifact_base.tar.gz"
    if [ -n "${TILO_CAPTURE_PINS:-}" ]; then
      platform="$out/$artifact_base-platform.tar.gz"
      python3 apps/imager/packaging/package-capture-platform.py \
        --host target/release/imager --output "$platform"
      write_digest "$platform"
      sign_linux_artifact "$platform"
    fi

    appdir="$out/Tilo_Imager.AppDir"
    mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
      "$appdir/usr/share/metainfo" \
      "$appdir/usr/share/icons/hicolor/scalable/apps"
    cp target/release/imager "$appdir/usr/bin/tilo-imager-bin"
    install -m755 target/release/tilo-enroll "$appdir/usr/bin/tilo-enroll"
    apps/imager/packaging/stage-capture-sidecar.sh target/release/imager "$appdir/usr" "$appdir/usr/bin/tilo-imager-bin"
    cp apps/imager/packaging/linux/tilo-imager "$appdir/usr/bin/tilo-imager"
    cp apps/imager/packaging/linux/tilo-imager.desktop \
      "$appdir/usr/share/applications/org.tilo.Imager.desktop"
    sed "s|https://github.com/tilo-os/releases|$release_page|g" \
      apps/imager/packaging/linux/org.tilo.Imager.metainfo.xml \
      >"$appdir/usr/share/metainfo/org.tilo.Imager.appdata.xml"
    cp apps/imager/packaging/linux/AppRun "$appdir/AppRun"
    cp apps/imager/packaging/linux/tilo-imager.svg "$appdir/tilo-imager.svg"
    cp apps/imager/packaging/linux/tilo-imager.svg "$appdir/.DirIcon"
    cp apps/imager/packaging/linux/tilo-imager.svg \
      "$appdir/usr/share/icons/hicolor/scalable/apps/tilo-imager.svg"
    chmod +x "$appdir/AppRun" "$appdir/usr/bin/tilo-imager" "$appdir/usr/bin/tilo-imager-bin"
    if command -v linuxdeploy >/dev/null 2>&1 && command -v appimagetool >/dev/null 2>&1; then
      linuxdeploy --appdir "$appdir" >/dev/null
      runtime_args=()
      if [ -n "${TILO_APPIMAGE_RUNTIME:-}" ]; then
        [ -f "$TILO_APPIMAGE_RUNTIME" ] \
          || { printf 'TILO_APPIMAGE_RUNTIME does not name a file\n' >&2; exit 1; }
        runtime_args=(--runtime-file "$TILO_APPIMAGE_RUNTIME")
      elif [ "$require_native_packages" = 1 ]; then
        printf 'TILO_APPIMAGE_RUNTIME is required; implicit runtime downloads are forbidden\n' >&2
        exit 1
      fi
      ARCH="$package_arch" appimagetool "${runtime_args[@]}" \
        "$appdir" "$out/$artifact_base.AppImage" >/dev/null
      chmod +x "$out/$artifact_base.AppImage"
      write_digest "$out/$artifact_base.AppImage"
      sign_linux_artifact "$out/$artifact_base.AppImage"
    elif [ "$require_native_packages" = 1 ]; then
      printf 'linuxdeploy and appimagetool are required for the release AppImage\n' >&2
      exit 1
    fi

    ;;
  *)
    printf 'unsupported host: %s\n' "$(uname -s)" >&2
    exit 2
    ;;
esac
