#!/usr/bin/env bash
# One action of the OS image (os/image/layers.toml), as t runs it.
#
#   build-layer.sh assets                        the checksum-pinned external assets, into
#                                                os/external-assets/cache (no root)
#   build-layer.sh base ARCH OUT                 the pinned base as the registry serves it,
#                                                into OUT/oci (no root)
#   build-layer.sh image NAME ARCH OUT           an image action's targets (the OS builder,
#                                                the side builds), each into OUT/<target>/oci
#   build-layer.sh layer NAME ARCH VARIANT OUT   a layer action: its piece's target built
#                                                over the image of every layer below, kept
#                                                as the fragment of the layers it added
#   build-layer.sh compose ARCH VARIANT OUT      the image of every layer, into OUT/oci, with
#                                                OUT/digest (no root)
#   build-layer.sh chunk ARCH OUT                the base and packages laid out by content (chunkah)
#                                                for the images devices update from
#   build-layer.sh changes ARCH OUT              what changed since main's previous image, failing
#                                                on layers that changed without their inputs
#   build-layer.sh disk ARCH OUT                 the development image installed onto a disk,
#                                                OUT/disk.qcow2, for VM tests and development VMs
#
# Image and layer actions run as root in a micro-VM step, in the OS assembler image
# (os/image/assembler.Containerfile: podman, skopeo, cosign and the tools build-image.sh
# verifies its inputs with, baked in rather than installed by every build). Each builds
# through os/dev/build-image.sh, which resolves and verifies the base, the RPM snapshot,
# the kernel and the other pins exactly as a whole build does, with the layers below
# given as images (TILO_LAYER_ARGS) instead of built again: a layer action reruns when
# its piece or what it reads changes, or a layer below it does, and never otherwise.
#
# What the actions it builds on left is under /target/os/ARCH, as their checks' outputs:
#   base/oci                     the base
#   builder/<target>/oci         the OS builder;  tools/<target>/oci  the side builds
#   compiled/                    the compiled tree (os/image/compile.sh)
#   layers/<layer>/              a fragment of the layers shared by every variant
#   VARIANT/layers/<layer>/      a fragment of the layers of one variant (trust and up)
#
# VARIANT is `dev` (the development loop and main's integration images: no LTO,
# identities derived from what they sign, the development owner key) or `release`
# (thin LTO, the release owner key, the identities a release signs with).
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/../../tools/ci/publish/lib.sh"
cd "$repo_root"

# Layers shared by every variant; the rest are the variant's.
shared_layers=" packages kernel system "
# The RFC 8032 test-vector key: public and non-production, accepted only for development and
# validation images (as tools/ci/publish/os-image.sh).
dev_owner_key=d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a

layers_in_order() {
  python3 -I -c 'import tomllib,sys; print(" ".join(l["name"] for l in tomllib.load(open(sys.argv[1],"rb"))["layer"]))' \
    os/image/layers.toml
}

layer_field() {
  python3 -I -c '
import tomllib, sys
plan = tomllib.load(open(sys.argv[1], "rb"))
for kind in ("layer", "image"):
    for l in plan.get(kind, []):
        if l["name"] == sys.argv[2]:
            v = l.get(sys.argv[3], "")
            print(" ".join(v) if isinstance(v, list) else str(v).lower() if isinstance(v, bool) else v)
' os/image/layers.toml "$1" "$2"
}

fragment_dir() {
  local name=$1
  case $shared_layers in
    *" $name "*) echo "$os/layers/$name" ;;
    *) echo "$os/$variant/layers/$name" ;;
  esac
}

# The guest's devices for podman: fuse for its storage, loop for the disk steps.
guest_devices() {
  modprobe fuse 2>/dev/null || true
  modprobe loop 2>/dev/null || true
  if [[ ! -c /dev/fuse ]]; then
    mount -t devtmpfs devtmpfs /dev
    mkdir -p /dev/pts /dev/shm
    [[ $(stat -f -c %T /dev/pts) == devpts ]] || mount -t devpts -o gid=5,mode=620,ptmxmode=666 devpts /dev/pts
    mountpoint -q /dev/shm || mount -t tmpfs -o mode=1777 tmpfs /dev/shm
  fi
  use_cosign
}

# The frozen RPM snapshot, as os-image.sh resolves it: the published one for this package
# set, else this run's image-rpms-ARCH, unsigned.
rpms_snapshot() {
  local built=/target/publish/rpms/$arch status=0 id
  bash os/dev/resolve-rpm-snapshot.sh --exists || status=$?
  case $status in
    0) RPMS_REF=$(bash os/dev/resolve-rpm-snapshot.sh) ;;
    1)
      [[ -f $built/image.oci.tar ]] || die "no published RPM snapshot for this package set and no image-rpms-$arch in this run"
      [[ $(archive_digest "$built/image.oci.tar") == "$(cat "$built/digest")" ]] || die "$built/image.oci.tar is not the build"
      id=$(podman pull -q "oci-archive:$built/image.oci.tar")
      RPMS_REF=localhost/tilo-rpms:$(bash os/dev/resolve-rpm-snapshot.sh --tag)-$arch
      podman tag "$id" "$RPMS_REF"
      export TILO_RPMS_TRUST=unsigned
      printf 'no published RPM snapshot for this package set: installing from image-rpms-%s, unsigned\n' "$arch" ;;
    *) die 'could not ask the registry for the published RPM snapshot' ;;
  esac
  export RPMS_REF
}

# Everything build-image.sh needs that the run decides: the pins, the clock, the variant.
build_env() {
  . os/base-snapshot.env
  export BASE_REF
  rpms_snapshot
  # One clock for every layer: the newest of the base's and the snapshot's creation, never
  # the commit's, so builds of different commits make the same bytes of the same inputs.
  SOURCE_DATE_EPOCH=$(bash os/dev/image-epoch.sh "$BASE_REF" "$RPMS_REF")
  TILO_GENERATION=$(TILO_ALLOW_EMPTY_SERIES=1 bash os/dev/next-generation.sh)
  export SOURCE_DATE_EPOCH TILO_GENERATION
  export TILO_IMAGE_ONLY=1 TILO_WITH_BROWSER=${TILO_WITH_BROWSER:-1} TILO_OUTPUT=$scratch/os-output TMPDIR=/var/tmp
  case $variant in
    dev)
      export TILO_RUST_LTO=off TILO_REQUIRE_PCR_PREDICTION=0 TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$dev_owner_key
      layer_args+=(--build-arg TILO_DERIVED_IDENTITIES=1) ;;
    release)
      [[ ${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:-} =~ ^[0-9a-fA-F]{64}$ ]] \
        || die 'a release image needs TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY, the release owner key'
      [[ ${TILO_DERIVED_IDENTITIES:-0} != 1 ]] || die 'a release never signs with derived development identities'
      export TILO_RUST_LTO=thin TILO_REQUIRE_PCR_PREDICTION=0
      [[ ${layer_name:-} != binaries ]] || release_identities ;;
    *) die "no variant $variant: dev or release" ;;
  esac
}

# A release signs its runtime bundle, its browser engine runtime and its apps with persistent
# release identities, t secrets of the trusted runs that build releases, so the same binaries
# give the same signed layers in every release build. Only the layers that pack them (the
# binaries) read them, as build secrets that never enter a layer.
release_identities() {
  local var id file pair missing=()
  for pair in TILO_RUNTIME_IDENTITY:runtime_bundle_identity TILO_BROWSER_RUNTIME_IDENTITY:browser_runtime_identity \
              TILO_APP_PUBLISHER_IDENTITY:app_publisher_identity; do
    var=${pair%%:*} id=${pair#*:}
    [[ -n ${!var:-} ]] || { missing+=("$var"); continue; }
    file=$scratch/$id.json
    ( umask 077; printf '%s' "${!var}" > "$file" )
    layer_args+=(--secret "id=$id,src=$file")
    # The browser runtime's identity is checked against its publisher key, public, so a wrong
    # secret fails the build rather than ship a runtime no device trusts.
    if [[ $id == browser_runtime_identity ]]; then
      [[ ${TILO_BROWSER_RUNTIME_PUBLISHER:-} =~ ^[0-9a-f]{64}$ ]] \
        || die 'TILO_BROWSER_RUNTIME_IDENTITY needs TILO_BROWSER_RUNTIME_PUBLISHER, its public key'
      layer_args+=(--build-arg "TILO_BROWSER_RUNTIME_PUBLISHER=$TILO_BROWSER_RUNTIME_PUBLISHER")
    fi
  done
  # Without them a release would mint keys per build, and its runtime and app layers would be
  # new in every release.
  ((${#missing[@]} == 0)) || die "a release signs with its release identities, and this run has no ${missing[*]} (t secrets of the runs that build releases)"
}

# An image layout into podman's storage; its id.
load() {
  podman pull -q "oci:$1"
}

# The image of the fragments' layers, loaded; its id. Composed into scratch, which is the
# step's own disk, then loaded into podman's storage on the same disk.
load_composition() {
  local dir
  dir=$(mktemp -d "$scratch/compose.XXXXXX")
  python3 -I tools/ci/os/oci_layers.py compose "$dir" "$@" >&2
  load "$dir/oci"
}

# The fragments of every layer below NAME (the base first), in order.
fragments_below() {
  local name=$1 l
  echo "$os/base/oci"
  for l in $(layers_in_order); do
    [[ $l == "$name" ]] && return 0
    fragment_dir "$l"
  done
  die "no layer $name in os/image/layers.toml"
}

# The fragments of every layer up to and including NAME.
fragments_through() {
  fragments_below "$1"
  fragment_dir "$1"
}

# Build TARGET of NAME's Containerfile through build-image.sh, given GIVEN (stage names) as
# images; tagged $image.
build_target() {
  local name=$1 target=$2 given=$3 file
  file=$scratch/$name.Containerfile
  python3 -I tools/ci/os/pieces.py containerfile "$name" --given "$given" > "$file"
  TILO_IMAGE=$image TILO_LAYER_TARGET=$target TILO_CONTAINERFILE=$file \
    TILO_LAYER_ARGS="${layer_args[*]}" bash os/dev/build-image.sh
}

cmd_assets() {
  bash os/dev/cache-github-assets.sh
}

cmd_base() {
  . os/base-snapshot.env
  rm -rf "$out/oci"
  mkdir -p "$out"
  # This machine's image of the base, its digest as the registry has it: its layers are
  # the blobs every image built on it shares.
  skopeo copy --quiet --preserve-digests --override-arch "$(oci_arch)" "docker://$BASE_REF" "oci:$out/oci:tilo"
  printf 'base %s for %s\n' "$BASE_REF" "$(oci_arch)"
}

oci_arch() {
  case $arch in
    x86_64) echo amd64 ;;
    aarch64) echo arm64 ;;
    *) die "no image for $arch" ;;
  esac
}

cmd_image() {
  local name=$1 target
  guest_devices
  build_env
  # The stages FROM ${BASE_REF} build on the base as its layout has it, not pulled again.
  podman tag "$(load "$os/base/oci")" localhost/tilo-base:pinned
  layer_args+=(--build-arg BASE_REF=localhost/tilo-base:pinned)
  for target in $(layer_field "$name" targets); do
    image=localhost/tilo-image:$name-$target
    build_target "$name" "$target" ""
    rm -rf "$out/$target"
    mkdir -p "$out/$target"
    podman push -q --compression-format gzip --force-compression "$image" "oci:$out/$target/oci:tilo"
    printf '%s: %s\n' "$target" "$(podman image inspect --format '{{.Digest}}' "$image")"
  done
}

cmd_layer() {
  local name=$1 target piece parent given="" entry stage what id inputs built also
  target=$(layer_field "$name" target)
  piece=$(layer_field "$name" piece)
  layer_name=$name
  guest_devices
  build_env
  # What the piece is given as images: other actions' images, and compositions of layers.
  for entry in $(layer_field "$name" given); do
    case $entry in
      compiled)
        # The variant's compile: without LTO for development, with thin LTO for a release.
        if [[ $variant == release ]]; then export TILO_COMPILED=$os/compiled-release; else export TILO_COMPILED=$os/compiled; fi
        continue ;;
      *:*) what=${entry%%:*} stage=${entry#*:} ;;
    esac
    if [[ -d $os/$what/$stage/oci ]]; then
      id=$(load "$os/$what/$stage/oci")
    elif [[ -d $(fragment_dir "$what")/also/$stage/oci ]]; then
      id=$(load "$(fragment_dir "$what")/also/$stage/oci")
    else
      mapfile -t frags < <(fragments_through "$what")
      id=$(load_composition "${frags[@]}")
    fi
    layer_args+=(--build-context "$stage=container-image://$id")
    given+=$stage,
  done
  # What it builds over: every layer below it, as one image under the name its piece
  # builds FROM. The bottom piece builds FROM ${BASE_REF}: the base, loaded from its layout.
  local below=-
  if [[ $(layer_field "$name" scratch) != true ]]; then
    parent=$(python3 -I tools/ci/os/pieces.py parent "$name")
    mapfile -t frags < <(fragments_below "$name")
    below=$scratch/below
    rm -rf "$below"
    python3 -I tools/ci/os/oci_layers.py compose "$below" "${frags[@]}"
    id=$(load "$below/oci")
    if [[ $parent == '${BASE_REF}' ]]; then
      podman tag "$id" localhost/tilo-base:pinned
      layer_args+=(--build-arg BASE_REF=localhost/tilo-base:pinned)
    else
      layer_args+=(--build-context "$parent=container-image://$id")
      given+=$parent,
    fi
    below=$below/oci
  else
    # The helper stages FROM ${BASE_REF} build on the base as its layout has it.
    id=$(load "$os/base/oci")
    podman tag "$id" localhost/tilo-base:pinned
    layer_args+=(--build-arg BASE_REF=localhost/tilo-base:pinned)
  fi
  image=localhost/tilo-layer:$name
  build_target "$name" "$target" "$given"
  built=$scratch/built
  rm -rf "$built"
  podman push -q --compression-format gzip --force-compression "$image" "oci:$built:tilo"
  # Its key, kept with its layers: the digest of every file its piece reads.
  inputs=$scratch/inputs.json
  mapfile -t globs < <(python3 -I tools/ci/os/pieces.py inputs "$name" --given "$given")
  python3 -I tools/ci/os/oci_layers.py inputs "${globs[@]}" > "$inputs"
  rm -rf "$out"
  python3 -I tools/ci/os/oci_layers.py take "$below" "$built" "$out" "$name" "$inputs"
  # Stages the actions above read beside the layers (the binaries' packing for the derived
  # state), from the same build's cache.
  for also in $(layer_field "$name" also); do
    image=localhost/tilo-layer:$name-$also
    build_target "$name" "$also" "$given"
    mkdir -p "$out/also/$also"
    podman push -q --compression-format gzip --force-compression "$image" "oci:$out/also/$also/oci:tilo"
  done
}

# The base and packages, laid out by content for updates: chunkah over their root (one
# component per RPM, from the image's own rpmdb; large files on their own; trees that change on
# their own schedule apart, os/scripts/rechunk-chunkah.py), proved to be the same tree
# (os/scripts/oci-rechunk.py), kept as the fragment that stands for both in the images devices
# update from. A snapshot that changes three packages then changes about three layers of it,
# and the layers above, built over the same tree, stack on it unchanged.
cmd_chunk() {
  local raw=$scratch/chunk-raw work=$scratch/chunk id frags
  guest_devices
  build_env
  mapfile -t frags < <(fragments_through packages)
  rm -rf "$raw" "$work"
  python3 -I tools/ci/os/oci_layers.py compose "$raw" "${frags[@]}"
  id=$(load "$raw/oci")
  bash os/dev/cache-github-assets.sh chunkah-0.7.1.tar.gz chunkah-0.7.1-vendor.tar.gz >&2
  mkdir -p "$work/context"
  podman build --security-opt label=disable -f os/Containerfile.rechunk --target rechunked \
    --build-context "rootfs=container-image://$id" \
    --build-context "github-assets=os/external-assets/cache" \
    --build-context "tilo-scripts=os/scripts" \
    --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" --build-arg TILO_RECHUNK_MAX_LAYERS=96 \
    --output "type=local,dest=$work/out" "$work/context"
  local finish=(--raw "oci:$raw/oci" --chunked "$work/out/image" --name localhost/tilo-packages:chunked)
  for tree in "$work"/out/trees/*/; do
    [[ -d $tree/image ]] && finish+=(--extra "${tree%/}/image")
  done
  python3 os/scripts/oci-rechunk.py finish "${finish[@]}" | tail -n1
  printf '{"inputs":{},"inputsDigest":""}' > "$scratch/chunk-inputs.json"
  rm -rf "$out"
  python3 -I tools/ci/os/oci_layers.py take - "$work/out/image" "$out" packages-chunked "$scratch/chunk-inputs.json"
}

# What changed since main's previous image (tilo-ci:main, which publish-os-ci moves): for each
# new layer, the action that made it and whether its inputs changed; a layer whose action's
# inputs did not change, yet whose bytes did, fails the check, named with the files that differ,
# as the nondeterminism it is: it costs every device an update. The previous image is read by
# its tag only to compare with; nothing is built from it.
cmd_changes() {
  local prev=$scratch/previous
  rm -rf "$prev" "$out"
  mkdir -p "$out"
  if ! skopeo copy --quiet --override-arch "$(oci_arch)" docker://registry.tilo.so/tilo/tilo-ci:main "oci:$prev:tilo"; then
    echo 'no previous main image to compare with (tilo-ci:main): nothing to say yet' | tee "$out/report.txt"
    return 0
  fi
  python3 -I tools/ci/os/oci_layers.py diff "$prev" "$os/image/oci" "$out/report.txt" --strict
}

# The image's disk: os-image-ARCH installed onto a qcow2 as a device installs it (bootc
# install, os/dev/disk-image.sh through build-image.sh), keyed by the image, so every VM test
# of the image and every development VM of it boots the same disk, made once.
cmd_disk() {
  local in=$os/image id
  guest_devices
  id=$(load "$in/oci")
  [[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$in/digest")" ]] || die "$in/oci is not the image $in/digest names"
  podman tag "$id" localhost/tilo:disk
  export TILO_IMAGE=localhost/tilo:disk TILO_SKIP_BUILD=1 TILO_DISTROBOX= TILO_OUTPUT=$scratch/disk-output TMPDIR=/var/tmp
  mkdir -p "$TILO_OUTPUT"
  bash os/dev/build-image.sh
  rm -rf "$out"
  mkdir -p "$out"
  # Compressed clusters: the disk is mostly empty, and what t stores and moves is its bytes.
  qemu-img convert -c -O qcow2 "$TILO_OUTPUT/qcow2/disk.qcow2" "$out/disk.qcow2"
  cp "$in/digest" "$out/image-digest"
  printf 'disk of %s: %s\n' "$(cat "$in/digest")" "$(du -h "$out/disk.qcow2" | cut -f1)"
}

cmd_compose() {
  local frags=() l
  # A release, which devices update from, takes the base and packages laid out by content
  # (`chunk`); the same tree, so the layers above stack on it as they were built.
  if [[ $variant == release && -f $os/layers/packages-chunked/fragment.json ]]; then
    frags+=("$os/layers/packages-chunked")
  else
    frags+=("$os/base/oci" "$os/layers/packages")
  fi
  for l in $(layers_in_order); do
    [[ $l == packages ]] && continue
    frags+=("$(fragment_dir "$l")")
  done
  . os/base-snapshot.env
  rm -rf "$out"
  mkdir -p "$out"
  SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0} python3 -I tools/ci/os/oci_layers.py compose "$out" "${frags[@]}"
}

layer_args=()
case ${1:-} in
  assets) cmd_assets ;;
  base) arch=${2:?} out=${3:?} os=/target/os/$arch; cmd_base ;;
  image) arch=${3:?} out=${4:?} os=/target/os/$arch variant=dev; cmd_image "${2:?}" ;;
  layer) arch=${3:?} variant=${4:?} out=${5:?} os=/target/os/$arch; [[ $(uname -m) == "$arch" ]] || die "the $arch layers build natively"; cmd_layer "${2:?}" ;;
  compose) arch=${2:?} variant=${3:?} out=${4:?} os=/target/os/$arch; cmd_compose ;;
  disk) arch=${2:?} out=${3:?} os=/target/os/$arch variant=dev; cmd_disk ;;
  chunk) arch=${2:?} out=${3:?} os=/target/os/$arch variant=release; cmd_chunk ;;
  changes) arch=${2:?} out=${3:?} os=/target/os/$arch; cmd_changes ;;
  *) die "usage: build-layer.sh assets | base ARCH OUT | image NAME ARCH OUT | layer NAME ARCH VARIANT OUT | compose ARCH VARIANT OUT | chunk ARCH OUT | changes ARCH OUT | disk ARCH OUT" ;;
esac
