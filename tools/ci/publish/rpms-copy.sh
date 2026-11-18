#!/usr/bin/env bash
# The copy half of the frozen RPM snapshot for this node's architecture: the snapshot the
# registry holds, by digest, into OUT as the OCI archive the OS layers load (OUT/image.oci.tar),
# with its digest (OUT/digest) and the clock it is dated by (OUT/epoch). Holds no secrets and
# asks the registry for exactly the digest SRC/ref names, so t serves an answer it made for an
# unchanged tree as an offline one.
#
# SRC is what rpms-build.sh left: SRC/ref, the digest-pinned reference to copy. Where it mirrored
# nothing (a branch, a working tree), SRC is the snapshot it built here and OUT is SRC: the
# archive is then the set already.
#
# usage: rpms-copy.sh ARCH SRC OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
arch=${1:?usage: rpms-copy.sh ARCH SRC OUT}
src=${2:?}
out=${3:?}
case $arch in
  x86_64 | aarch64) ;;
  *) die "unknown architecture $arch" ;;
esac
cd "$repo_root"
mkdir -p "$out"
# The newest signature among the packages of the snapshot archive $1, as seconds: the clock an OS
# build from this snapshot takes (with its base's date), which rpm refuses a package signed after.
# The image itself is dated when it was resolved, which a build of the same packages minutes later
# moves, and with it every layer built on it; this moves only with the packages.
snapshot_epoch() {
  local dir blob epoch
  dir=$(mktemp -d "$scratch/snapshot.XXXXXX")
  mkdir "$dir/oci" "$dir/root"
  tar -C "$dir/oci" -xf "$1"
  for blob in "$dir"/oci/blobs/sha256/*; do
    [[ $(head -c 2 "$blob" | od -An -tx1 | tr -d ' \n') == 1f8b ]] || continue
    tar -C "$dir/root" -xzf "$blob"
  done
  epoch=$(LC_ALL=C TZ=UTC rpm -qp --nosignature --nodigest --qf '%{RSAHEADER:pgpsig}\n' "$dir"/root/repo/*.rpm \
    | sed -n 's/^[^,]*, \(.*\), Key ID .*/\1/p' | TZ=UTC date -f - +%s | sort -n | tail -n 1)
  rm -rf -- "$dir"
  [[ $epoch =~ ^[1-9][0-9]*$ ]] || die "no signed package in $1 to date the snapshot by"
  printf '%s\n' "$epoch"
}

if [[ -f $src/ref ]]; then
  ref=$(cat "$src/ref")
  [[ $ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] || die "unexpected snapshot $ref"
  digest=${ref##*@}
  # skopeo copies blobs and manifest verbatim, so the layout holds the published digest; buildah
  # rewrites the manifest, which would make this a second snapshot of the same package set. The
  # layout is packed here rather than by skopeo's oci-archive:, whose tar carries the time of the
  # copy: the same snapshot left a different archive in every copy, and every check that needs
  # this one was keyed anew by it (t keys a check by its needs' outputs).
  layout=$(mktemp -d "$scratch/published.XXXXXX")
  skopeo copy --quiet --retry-times 3 "docker://$ref" "oci:$layout"
  rm -f "$out/image.oci.tar"
  tar -C "$layout" --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner --format=gnu \
    --mode=u=rwX,go=rX -cf "$out/image.oci.tar" oci-layout index.json blobs
  rm -rf -- "$layout"
  [[ $(archive_digest "$out/image.oci.tar") == "$digest" ]] \
    || die "the copy of $digest is not the published image"
  printf '%s\n' "$digest" > "$out/digest"
  snapshot_epoch "$out/image.oci.tar" > "$out/epoch"
  printf 'copied the snapshot %s (%s); dated %s by its newest package signature\n' \
    "$ref" "$arch" "$(cat "$out/epoch")"
  exit 0
fi

# This architecture's set is not in the registry: the snapshot this job built is the set, as it
# has always been for a branch or a working tree.
[[ -s $src/image.oci.tar && -f $src/digest ]] \
  || die "$src names neither a snapshot reference nor a snapshot this job built"
[[ $(archive_digest "$src/image.oci.tar") == "$(cat "$src/digest")" ]] \
  || die "$src/image.oci.tar is not the build $(cat "$src/digest")"
if [[ $(realpath -- "$src") != "$(realpath -- "$out")" ]]; then
  cp --reflink=auto -f -- "$src/image.oci.tar" "$out/image.oci.tar"
  cp -f -- "$src/digest" "$out/"
fi
if [[ -f $src/epoch ]]; then
  [[ $(realpath -- "$src") == "$(realpath -- "$out")" ]] || cp -f -- "$src/epoch" "$out/epoch"
else
  snapshot_epoch "$out/image.oci.tar" > "$out/epoch"
fi
printf 'kept the snapshot this job built (%s); dated %s by its newest package signature\n' \
  "$(cat "$out/digest")" "$(cat "$out/epoch")"
