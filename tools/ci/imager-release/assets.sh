#!/usr/bin/env bash
# The standalone installer's complete release, gathered from the platform checks
# into one directory and checked before anything is published.
#
#   assets.sh gather <out> <version>   copy every platform's downloads into <out>,
#                                      write the WinGet manifests, then verify
#   assets.sh verify <dir>             require the complete matrix and its checksums
#
# A release tag (T_REF=tag:imager-v*) takes the signed outputs and requires every
# signature; main's newest head takes the unsigned packages: the dry run, which
# produces and checks the same files without publishing them.
set -euo pipefail

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
signed=0
case "${T_REF:-}" in tag:imager-v*) signed=1 ;; esac

required() {
  local arch
  for arch in x86_64 aarch64; do
    printf '%s\n' "tilo-installer-linux-$arch.img.zst" \
      "tilo-imager-linux-$arch.AppImage" "tilo-imager-linux-$arch.tar.gz" "tilo-imager-linux-$arch.flatpak" \
      "tilo-imager-windows-$arch.exe" "tilo-imager-windows-$arch.zip" "tilo-imager-setup-windows-$arch.exe" \
      "tilo-imager-macos-$arch.zip" "tilo-imager-macos-$arch.dmg"
  done
}

verify() {
  local dir="$1" name
  while read -r name; do
    [ -s "$dir/$name" ] || { printf 'missing release artifact: %s\n' "$name" >&2; exit 1; }
    [ -s "$dir/$name.sha256" ] || { printf 'missing release digest: %s.sha256\n' "$name" >&2; exit 1; }
    (cd "$dir" && sha256sum --check --strict --quiet "$name.sha256") \
      || { printf 'invalid release digest: %s.sha256\n' "$name" >&2; exit 1; }
    if [ "$signed" = 1 ]; then
      case "$name" in
        *.img.zst|*.AppImage|*.tar.gz|*.flatpak)
          [ -s "$dir/$name.asc" ] || { printf 'missing release signature: %s.asc\n' "$name" >&2; exit 1; } ;;
      esac
    fi
  done < <(required)
  for name in Tilo.Imager.installer.yaml Tilo.Imager.yaml Tilo.Imager.locale.en-US.yaml; do
    [ -n "$(find "$dir" -type f -name "$name" -print -quit)" ] \
      || { printf 'missing WinGet manifest: %s\n' "$name" >&2; exit 1; }
  done
  printf 'the installer release in %s is complete: %s files\n' "$dir" "$(required | wc -l)"
}

gather() {
  local out="$1" version="$2" stage arch
  stage=package
  [ "$signed" = 1 ] && stage=signed
  rm -rf "$out" && mkdir -p "$out"
  for arch in x86_64 aarch64; do
    cp -- /target/linux/"$arch"/"$stage"/tilo-imager-linux-"$arch".* "$out/"
    cp -- /target/windows/"$arch"/"$stage"/tilo-imager-*windows-"$arch".* "$out/"
    cp -- /target/macos/"$arch"/"$stage"/tilo-imager-macos-"$arch".* "$out/"
    cp -- /target/imager-medium/"$arch"/tilo-installer-linux-"$arch".img.zst* "$out/"
  done
  # The release key's public half, which verify-linux-release.sh and the site read.
  [ "$signed" = 0 ] || cp -- /target/linux/x86_64/signed/tilo-release-key.* "$out/"
  python3 "$repo/apps/imager/packaging/generate-winget.py" "$out" --version "$version" \
    --base-url "https://github.com/tilo-os/releases/releases/download/imager-v$version"
  python3 "$repo/apps/imager/packaging/validate-winget.py" "$out/winget"
  verify "$out"
}

case "${1:-}" in
  gather) gather "${2:?output directory}" "${3:?version}" ;;
  verify) verify "${2:?release directory}" ;;
  *) printf 'usage: assets.sh gather <out> <version> | verify <dir>\n' >&2; exit 2 ;;
esac
