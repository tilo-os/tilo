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
      export TILO_RUST_LTO=thin TILO_REQUIRE_PCR_PREDICTION=0 ;;
    *) die "no variant $variant: dev or release" ;;
  esac
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

cmd_compose() {
  local frags=() l
  frags+=("$os/base/oci")
  for l in $(layers_in_order); do frags+=("$(fragment_dir "$l")"); done
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
  *) die "usage: build-layer.sh assets | base ARCH OUT | image NAME ARCH OUT | layer NAME ARCH VARIANT OUT | compose ARCH VARIANT OUT" ;;
esac
