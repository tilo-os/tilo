#!/usr/bin/env bash
# Build a Linux Imager whose compiled-in catalog names one exported internal
# installer bundle, for the USB-free VM round trip only.
#
#   ./os/dev/build-internal-cycle-imager.sh EXPORT_DIR RELEASE_TAG OUTPUT_BINARY
#
#     EXPORT_DIR     output of os/dev/export-internal-bundle.sh DISK ARCH RELEASE_TAG
#                    EXPORT_DIR (holds internal-catalog-ARCH.json + bundle/)
#     RELEASE_TAG    imager-vMAJOR.MINOR.PATCH; must be the RELEASE_ID the bundle
#                    was exported with (the catalog binds release_id == tag)
#     OUTPUT_BINARY  where the built `imager` is copied (never overwritten)
#
# Why this exists: the elevated staging helper resolves a request's release
# ONLY against apps/imager/release-catalog.json, embedded at compile time
# (release.rs `include_str!`), and then requires the manifest file's SHA-256
# to equal that entry. A test bundle is therefore usable only by a binary
# built with a catalog that names it. This script produces that catalog with
# the production merger, exactly as the release catalog job does:
#
#   1. copy the bundle's internal-catalog-ARCH.json and its manifest into a
#      scratch asset tree (build-release-catalog.sh requires the manifest to
#      sit beside its fragment and re-validates its bytes and schema);
#   2. add one placeholder media fragment per architecture. The merger insists
#      on the generic x86_64 + aarch64 media matrix; these entries point at
#      https://code.tilo.so/tilo/internal-cycle-placeholder/... files that do
#      not exist and are never downloaded (the round trip never writes media);
#   3. run apps/imager/packaging/build-release-catalog.sh with
#      GITHUB_REF_NAME=RELEASE_TAG, TILO_RELEASE_SERVER=https://code.tilo.so
#      (the only origin the Imager accepts) and the same bundle owner;
#   4. embed it with apps/imager/packaging/release-catalog.py --copy-to, build
#      `cargo build --release --locked -p imager --bin imager`, and restore the
#      tracked catalog afterwards.
#
# The resulting binary is a test artifact: its catalog carries placeholder
# media and must never be published.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { printf 'build-internal-cycle-imager: %s\n' "$*" >&2; exit 1; }
[ "$#" -eq 3 ] || { sed -n '2,33p' "$0" >&2; exit 2; }
export_dir="$1" tag="$2" output="$3"
[[ "$tag" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "release tag must be imager-vMAJOR.MINOR.PATCH: $tag"
[ ! -e "$output" ] || die "output exists: $output"
for tool in jq python3 cargo sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || die "missing required tool: $tool"
done
mapfile -t fragments < <(find "$export_dir" -maxdepth 1 -type f -name 'internal-catalog-*.json')
[ "${#fragments[@]}" -eq 1 ] || die "expected exactly one internal-catalog-ARCH.json in $export_dir"
fragment="${fragments[0]}"
arch="$(jq -r .arch "$fragment")"
case "$arch" in x86_64|aarch64) ;; *) die "fragment names an unsupported architecture: $arch" ;; esac
manifest="$export_dir/bundle/tilo-internal-installer-$arch.json"
[ -f "$manifest" ] && [ ! -L "$manifest" ] || die "missing exported manifest: $manifest"
[ "$(jq -r .release_id "$fragment")" = "$tag" ] \
  || die "the bundle was exported as $(jq -r .release_id "$fragment"), not $tag"
manifest_sha256="$(sha256sum "$manifest" | awk '{print $1}')"
[ "$(jq -r .manifest_sha256 "$fragment")" = "$manifest_sha256" ] || die "fragment does not bind the manifest bytes"

work="$(mktemp -d "${TMPDIR:-/tmp}/tilo-internal-cycle-catalog.XXXXXX")"
catalog="$repo/apps/imager/release-catalog.json"
backup="$work/release-catalog.json.tracked"
cp -p "$catalog" "$backup"
restore() { cp -p "$backup" "$catalog"; rm -rf "$work"; }
trap restore EXIT

mkdir -p "$work/assets/internal/bundle" "$work/assets/media"
cp "$fragment" "$work/assets/internal/"
cp "$manifest" "$work/assets/internal/bundle/"
server=https://code.tilo.so
repository=tilo/internal-cycle-placeholder
placeholder="$(printf 'tilo internal-install-cycle placeholder medium' | sha256sum | awk '{print $1}')"
for media_arch in x86_64 aarch64; do
  jq -n --arg arch "$media_arch" \
    --arg url "$server/$repository/releases/download/$tag/tilo-installer-linux-$media_arch.img.zst" \
    --arg digest "$placeholder" \
    '{name:"Tilo (internal install cycle placeholder)",version:"0.0.0",channel:"internal-cycle",
      arch:$arch,url:$url,archive_sha256:$digest,archive_size:1,image_sha256:$digest,image_size:1}' \
    > "$work/assets/media/catalog-$media_arch.json"
done
GITHUB_REF_NAME="$tag" TILO_RELEASE_SERVER="$server" TILO_RELEASE_REPOSITORY="$repository" \
  TILO_INTERNAL_BUNDLE_OWNER="${TILO_INTERNAL_BUNDLE_OWNER:-tilo}" \
  "$repo/apps/imager/packaging/build-release-catalog.sh" "$work/assets" "$work/catalog" >/dev/null
jq -e --arg sha "$manifest_sha256" --arg arch "$arch" \
  '[.releases[] | select(.purpose == "tilo-internal-installer" and .arch == $arch and .manifest_sha256 == $sha)] | length == 1' \
  "$work/catalog"/tilo-installer-catalog-v*.json >/dev/null || die "merged catalog does not name the bundle"
python3 "$repo/apps/imager/packaging/release-catalog.py" "$work/catalog" --copy-to "$catalog"

(cd "$repo" && cargo build --release --locked -p imager --bin imager --features installation)
binary="$repo/target/release/imager"
[ -x "$binary" ] || die "cargo did not produce $binary"
# The embedded catalog is plain JSON inside the binary; prove it is this one.
grep -aFq "$manifest_sha256" "$binary" || die "built imager does not embed the bundle's manifest digest"
install -m 0755 "$binary" "$output"
printf '%s  %s\n' "$(sha256sum "$output" | awk '{print $1}')" "$output"
