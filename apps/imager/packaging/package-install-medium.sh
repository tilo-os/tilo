#!/usr/bin/env bash
# Convert the tested live-installer qcow2 to a compressed raw image that the
# cross-platform Imager can expand and write byte-for-byte.
set -euo pipefail

release_tag="${QOS_INSTALLER_RELEASE_TAG:-${GITHUB_REF_NAME:-}}"
release_repository="${QOS_RELEASE_REPOSITORY:-qos/releases}"
release_server="${GITHUB_SERVER_URL:-https://code.sandbox.example}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[[ "$release_server" =~ ^https://[^/]+$ ]] \
  || { printf 'release server must be an HTTPS origin\n' >&2; exit 2; }
version="${QOS_INSTALLER_VERSION:-${release_tag#imager-v}}"
channel="${QOS_INSTALLER_CHANNEL:-stable}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'installer version must be MAJOR.MINOR.PATCH\n' >&2; exit 2; }
case "$channel" in
  stable)
    [ "$release_tag" = "imager-v$version" ] \
      || { printf 'stable installer tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
    ;;
  rehearsal)
    [ -n "${QOS_INSTALLER_RELEASE_TAG:-}" ] \
      && [ -n "${QOS_INSTALLER_VERSION:-}" ] \
      && [[ "$release_tag" =~ ^rehearsal-[0-9a-fA-F]{40}$ ]] \
      || { printf 'rehearsal media require an explicit source-bound tag and version\n' >&2; exit 2; }
    ;;
  *) printf 'installer channel must be stable or rehearsal\n' >&2; exit 2 ;;
esac

input="${1:?usage: package-install-medium.sh INSTALLER.qcow2 ARCH [OUT]}"
arch="${2:?missing architecture}"
out="${3:-dist/qos-installer}"
case "$arch" in x86_64|aarch64) ;; *) printf 'unsupported architecture: %s\n' "$arch" >&2; exit 2 ;; esac
command -v qemu-img >/dev/null
command -v zstd >/dev/null
command -v jq >/dev/null
test -s "$input"

mkdir -p "$out"
base="qos-installer-linux-${arch}.img"
raw="$out/$base"
archive="$raw.zst"
qemu-img convert -p -f qcow2 -O raw "$input" "$raw"
image_size="$(stat -c %s "$raw")"
image_sha256="$(sha256sum "$raw" | awk '{print $1}')"
zstd --threads=0 --ultra -19 --sparse -f "$raw" -o "$archive"
archive_sha256="$(sha256sum "$archive" | awk '{print $1}')"
archive_size="$(stat -c %s "$archive")"
(cd "$out" && sha256sum "$(basename "$archive")") >"$archive.sha256"
rm -f "$raw"

jq -n \
  --arg name 'qOS' \
  --arg version "$version" \
  --arg channel "$channel" \
  --arg arch "$arch" \
  --arg url "${release_server}/${release_repository}/releases/download/${release_tag}/$(basename "$archive")" \
  --arg archive_sha256 "$archive_sha256" \
  --argjson archive_size "$archive_size" \
  --arg image_sha256 "$image_sha256" \
  --argjson image_size "$image_size" \
  '{name:$name,version:$version,channel:$channel,arch:$arch,url:$url,archive_sha256:$archive_sha256,archive_size:$archive_size,image_sha256:$image_sha256,image_size:$image_size}' \
  >"$out/catalog-${arch}.json"
