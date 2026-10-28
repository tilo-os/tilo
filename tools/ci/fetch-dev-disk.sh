#!/usr/bin/env bash
# Main's published dev disk, as t's dev-disk-base-ARCH check fetches it: the disk
# publish-os-dev-disk-ARCH published after os-acceptance passed on main, named by t's
# `dev-disk-ARCH` pointer (T_POINTER_DEV_DISK_<ARCH>: the disk manifest
# os/dev/publish-dev-disk.sh wrote). It is vm-acceptance-layers' base, over which a
# working tree's userspace is swapped in (tools/ci/vm-acceptance.sh layers).
#
#   fetch-dev-disk.sh ARCH OUT   OUT/disk/{disk.qcow2,image-digest,source-revision,manifest.json}
set -euo pipefail

arch=${1:?usage: fetch-dev-disk.sh ARCH OUT}
out=${2:?usage: fetch-dev-disk.sh ARCH OUT}
var=T_POINTER_DEV_DISK_${arch^^}
manifest=${!var:-}
[[ -n $manifest ]] || {
  echo "no dev-disk-$arch pointer: main has published no dev disk yet (publish-os-dev-disk-$arch runs" \
       "after os-acceptance-$arch passes on main); test with the full disk instead: t test vm-acceptance" >&2
  exit 1
}
rm -rf "$out"
mkdir -p "$out/disk"
printf '%s\n' "$manifest" > "$out/disk/manifest.json"
pick() { jq -er --arg arch "$arch" ".disks[] | select(.format == \"qcow2\" and .arch == \$arch) | .$1" "$out/disk/manifest.json"; }
url=$(pick url)
sha=$(pick sha256)
jq -er .imageDigest "$out/disk/manifest.json" > "$out/disk/image-digest"
jq -er .sourceRevision "$out/disk/manifest.json" > "$out/disk/source-revision"
echo "fetching main's dev disk of $(cut -c1-12 "$out/disk/source-revision") ($(cat "$out/disk/image-digest")) from $url"
curl -fsSL --retry 3 -o "$out/disk/disk.qcow2" "$url"
echo "$sha  $out/disk/disk.qcow2" | sha256sum -c --quiet \
  || { echo "the fetched disk does not match the manifest's sha256 $sha" >&2; exit 1; }
