#!/usr/bin/env bash
# The development server image (os/Containerfile.host-dev): the server image image-host built
# (HOST, before it is sealed) with the dev-VM daemon and CLI t's host-dev-binaries check compiled
# (BINARIES, server-binaries.sh host-dev), tilo-vmd's boot units and a base.json describing how the
# server's own programs were built. seal-host-dev seals it as seal-host seals the server image.
#
# The check image that compiled both sets of programs is t.toml's default one: host-binaries and
# host-dev-binaries name no image of their own, which this refuses to assume without checking.
# A layer `tilo-vm layer-pack` builds in an ordinary check comes from that same image, target and
# toolchain, so the daemon on an installed development server admits it.
#
# usage: host-dev-build.sh BINARIES HOST OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: host-dev-build.sh BINARIES HOST OUT'
binaries=${1:?$usage}
host=${2:?$usage}
out=${3:?$usage}
cd "$repo_root"
# tilo-vmd's units a development server enables: the record of the booted image (before
# systemd-sysext merges anything), the accounts brought back after boot, and the layers an
# installation left for the first boot, applied before the server starts.
vm_units=(tilo-vm-reconcile-image.service tilo-vm-restore.service tilo-vm-incoming.service)
for unit in "${vm_units[@]}"; do
  [[ -f crates/dev/tilo-vmd/data/$unit ]] || die "no crates/dev/tilo-vmd/data/$unit"
done
mapfile -t bins < <(python3 os/scripts/install-components.py --dry-run --format bins vmd vm-cli)
for bin in "${bins[@]}"; do
  [[ -x $binaries/$bin ]] || die "no $bin in $binaries"
done

# t.toml's default image: the first `image =` before the first table.
builder=$(awk '/^\[/ { exit } /^image = "/ { gsub(/^image = "|"$/, ""); print; exit }' t.toml)
[[ $builder =~ ^registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}$ ]] \
  || die "t.toml names no default check image pinned by digest: '$builder'"
for check in host-binaries host-dev-binaries; do
  awk -v name="name = \"$check\"" '
    /^\[\[check\]\]/ { inside = 0 } $0 == name { inside = 1 }
    inside && /^image = / { found = 1 } END { exit !found }' t.toml \
    && die "$check names an image of its own; base.json would name the wrong builder"
done

use_buildah buildah overlay
[[ -s $host/rpms-source && -s $host/generation ]] || die "$host is not an image-host build"
host_digest=$(cat "$host/digest")
[[ $(archive_digest "$host/image.oci.tar") == "$host_digest" ]] \
  || die "$host/image.oci.tar is not the image $host_digest names"
b tag "$(b pull -q "oci-archive:$host/image.oci.tar")" localhost/tilo-host:built
created=$(b inspect --type image --format '{{.OCIv1.Created.UTC.Format "2006-01-02T15:04:05Z"}}' \
  localhost/tilo-host:built)
# Dated as the server image is, so the same server and the same additions make the same image.
SOURCE_DATE_EPOCH=$(b inspect --type image --format '{{.OCIv1.Created.Unix}}' localhost/tilo-host:built)
export SOURCE_DATE_EPOCH

context=$scratch/host-dev-context
rm -rf "$context"
mkdir -p "$context/bin" "$context/vm-units" "$context/os/scripts"
cp os/Containerfile.host-dev "$context/Containerfile"
for bin in "${bins[@]}"; do
  cp "$binaries/$bin" "$context/bin/"
done
for unit in "${vm_units[@]}"; do
  cp "crates/dev/tilo-vmd/data/$unit" "$context/vm-units/"
done
cp os/components.toml "$context/os/"
cp os/scripts/install-components.py os/scripts/finalize-dev-image.py "$context/os/scripts/"
cp Cargo.lock "$context/"
bash tools/ci/publish/oci-build.sh "$context" "$context/Containerfile" "$out" \
  --platform linux/amd64 \
  --build-arg "HOST_IMAGE=localhost/tilo-host:built" \
  --build-arg "TILO_BASE_DIGEST=$host_digest" \
  --build-arg "TILO_BASE_CREATED=$created" \
  --build-arg "TILO_BUILDER_IMAGE=$builder" \
  --build-arg "TILO_VM_UNITS=${vm_units[*]}"
cp "$host/rpms-source" "$host/generation" "$out/"
# What the image's layer check reads, beside it, for whoever builds a layer for it.
b tag "$(b pull -q "oci-archive:$out/image.oci.tar")" localhost/tilo-host-dev:built
container=$(b from -q localhost/tilo-host-dev:built)
cp "$(b mount "$container")/usr/share/tilo-vm/base.json" "$out/base.json"
b rm "$container" >/dev/null
printf 'development server %s on %s\n' "$(cat "$out/digest")" "$host_digest"
cat "$out/base.json"
