#!/usr/bin/env bash
# One action of the OS image (os/image/layers.toml), as t runs it.
#
#   build-layer.sh base ARCH OUT                 the pinned base as the registry serves it,
#                                                into OUT/oci (no root)
#   build-layer.sh image NAME ARCH OUT           an image action's targets (the OS builder,
#                                                the side builds), each into OUT/<target>/oci
#   build-layer.sh layer NAME ARCH OUT           a layer action: its piece's target built
#                                                over the image of every layer below, kept
#                                                as the fragment of the layers it added
#   build-layer.sh compose ARCH OUT              the image of every layer, into OUT/oci, with
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
# What the actions it builds on left is under /target/os/ARCH (TILO_OS_ROOT/ARCH, for a build of
# them all in one step), as their checks' outputs:
#   base/oci                     the base
#   builder/<target>/oci         the OS builder;  tools/<target>/oci  the side builds
#   compiled/                    the compiled tree (os/image/compile.sh); a release's in
#                                compiled-release/
#   chromium/                    the browser's engine, for a compiled tree without it
#   layers/<layer>/              the fragment of the layers that layer action added; a
#                                release's own layers in release-layers/<layer>/, a board
#                                release's in board-layers/<board>/<layer>/
#
# The image is a development image: no LTO, the development owner key, its UKI sealing a
# placeholder composefs digest. With TILO_OS_FLAVOUR=release it is the image a release seals and
# signs (tools/ci/publish/os-release.sh): thin LTO from compiled-release/, the registry owner's
# key (os/publication.env); the layers marked `release` in os/image/layers.toml, which those
# change, are built again into release-layers/, the others are the development image's. With
# TILO_OS_BOARD=<board> as well it is that board's release image: the layers marked `board`,
# which read the hardware profile, are built again with it into board-layers/<board>/, with the
# board's firmware (os/publication.env), its update origin (the tag of this commit's board release,
# code.tilo.so/tilo/tilo:<board>-<commit>) and the generation of its series
# (os/dev/board-generation.sh).
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/../../tools/ci/publish/lib.sh"
cd "$repo_root"

# The RFC 8032 test-vector key: public and non-production, accepted only for development and
# validation images (as tools/ci/publish/os-image.sh).
dev_owner_key=d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a

# How long each part of an action took, for finding what a build spends its time on.
phase_started=$SECONDS phase_name=start
phase() {
  printf 'TILO_LAYER_TIMING phase=%s seconds=%s\n' "$phase_name" "$((SECONDS - phase_started))"
  phase_name=$1 phase_started=$SECONDS
}
trap 'phase end' EXIT

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

board_layer() {
  [[ -n $board && $(layer_field "$1" board) == true ]]
}

fragment_dir() {
  if board_layer "$1"; then
    echo "$os/board-layers/$board/$1"
  elif [[ $flavour == release && $(layer_field "$1" release) == true ]]; then
    echo "$os/release-layers/$1"
  else
    echo "$os/layers/$1"
  fi
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
# set, else this run's image-rpms-ARCH, unsigned. Loaded only for an action that installs
# from it (rpms_read=1); one that doesn't, such as the binaries' and the derived state's, is
# given only its name and the clock it sets (RPMS_CREATED), read from the archive, rather than
# gigabytes of packages to unpack into its storage.
rpms_read=1
rpms_snapshot() {
  local built=/target/publish/rpms/$arch status=0 id
  RPMS_CREATED=
  bash os/dev/resolve-rpm-snapshot.sh --exists || status=$?
  case $status in
    0) RPMS_REF=$(bash os/dev/resolve-rpm-snapshot.sh) ;;
    1)
      [[ -f $built/image.oci.tar ]] || die "no published RPM snapshot for this package set and no image-rpms-$arch in this run"
      [[ $(archive_digest "$built/image.oci.tar") == "$(cat "$built/digest")" ]] || die "$built/image.oci.tar is not the build"
      RPMS_REF=localhost/tilo-rpms:$(bash os/dev/resolve-rpm-snapshot.sh --tag)-$arch
      if [[ $rpms_read == 1 ]]; then
        id=$(podman pull -q "oci-archive:$built/image.oci.tar")
        podman tag "$id" "$RPMS_REF"
      else
        RPMS_CREATED=$(archive_created "$built/image.oci.tar")
      fi
      export TILO_RPMS_TRUST=unsigned
      printf 'no published RPM snapshot for this package set: installing from image-rpms-%s, unsigned\n' "$arch" ;;
    *) die 'could not ask the registry for the published RPM snapshot' ;;
  esac
  export RPMS_REF
}

# The creation time of the single image in the OCI archive $1, as seconds (what
# os/dev/image-epoch.sh reads of a loaded image: its configuration's `created`).
archive_created() {
  python3 -I - "$1" <<'PY'
import datetime, json, sys, tarfile
with tarfile.open(sys.argv[1]) as t:
    names = {m.name.removeprefix("./"): m for m in t}
    blob = lambda d: json.load(t.extractfile(names["blobs/sha256/" + d.split(":", 1)[1]]))
    doc = json.load(t.extractfile(names["index.json"]))
    while "config" not in doc:
        doc = blob(doc["manifests"][0]["digest"])
    created = blob(doc["config"]["digest"])["created"]
print(int(datetime.datetime.fromisoformat(created.replace("Z", "+00:00")).timestamp()))
PY
}

# Whether layer NAME's piece, given the stages it is given as images, installs from the RPM
# snapshot: its Containerfile, with the helpers it builds, names the snapshot's stage.
reads_rpms() {
  python3 -I - "$1" <<'PY'
import sys
sys.path.insert(0, "tools/ci/os")
import pieces
name = sys.argv[1]
layer = pieces.layer(name)
given = {g.split(":", 1)[1] for g in layer.get("given", []) if g != "compiled"}
if not layer.get("scratch"):
    given.add(pieces.parent(name))
refs = pieces.references(pieces.containerfile(name, given))
sys.exit(0 if refs & {"rpms", "${RPMS_REF}"} else 1)
PY
}

# Everything build-image.sh needs that the run decides: the pins, the clock, the keys.
build_env() {
  . os/base-snapshot.env
  export BASE_REF
  rpms_snapshot
  # One clock for every layer: the newest of the base's and the snapshot's creation, never
  # the commit's, so builds of different commits make the same bytes of the same inputs.
  if [[ -n $RPMS_CREATED ]]; then
    SOURCE_DATE_EPOCH=$(bash os/dev/image-epoch.sh "$BASE_REF")
    if (( RPMS_CREATED > SOURCE_DATE_EPOCH )); then SOURCE_DATE_EPOCH=$RPMS_CREATED; fi
  else
    SOURCE_DATE_EPOCH=$(bash os/dev/image-epoch.sh "$BASE_REF" "$RPMS_REF")
  fi
  TILO_GENERATION=$(TILO_ALLOW_EMPTY_SERIES=1 bash os/dev/next-generation.sh)
  export SOURCE_DATE_EPOCH TILO_GENERATION
  export TILO_IMAGE_ONLY=1 TILO_WITH_BROWSER=${TILO_WITH_BROWSER:-1} TILO_OUTPUT=$scratch/os-output TMPDIR=/var/tmp
  export TILO_REQUIRE_PCR_PREDICTION=0
  if [[ $flavour == release ]]; then
    export TILO_RUST_LTO=thin TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$(publication TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY)
  else
    export TILO_RUST_LTO=off TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$dev_owner_key
  fi
  # The pinned external assets are os-assets' outputs, which the action needs.
  export TILO_ASSET_OFFLINE=1
}

# What a board layer builds with besides: the board's firmware, staged where the pieces bind it
# from, its profile, its update origin and the generation of its own series.
board_env() {
  stage_board_firmware "$board"
  TILO_TARGET_IMGREF=code.tilo.so/tilo/tilo:$board-$(git rev-parse --verify HEAD) \
    || die 'a board release names its commit: git = true'
  TILO_GENERATION=$(bash os/dev/board-generation.sh)
  export TILO_HARDWARE_PROFILE=$board TILO_TARGET_IMGREF TILO_GENERATION
}

# An image layout into podman's storage; its id.
load() {
  podman pull -q "oci:$1"
}

# The image of the fragments' layers, loaded; its id. A view of them in scratch, its blobs
# links to the fragments' own (on the step's read-only outputs disk, which a copy would
# duplicate onto the step's own), loaded into podman's storage.
load_composition() {
  local dir
  dir=$(mktemp -d "$scratch/compose.XXXXXX")
  python3 -I tools/ci/os/oci_layers.py view "$dir" "$@" >&2
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

# A layer over others is dated as they are: by the clock the layer below it was built with (its
# org.tilo.source-date-epoch label), not by one read again from this run's RPM snapshot. The
# snapshot is dated when it was resolved, and t serves the layers below by their keys, which a
# snapshot built again under the same key with a later date doesn't change: a layer dated anew
# then stamped its packages' install times after those of the layers it builds on, and the
# image's checks (tilo-verify-rpm-files) found no package installed at its clock. So every layer
# of an image takes the clock of the bottom one, which read it from the snapshot it installed.
below_dated() {
  local epoch
  epoch=$(python3 -I - "$below" <<'PY'
import sys
sys.path.insert(0, "tools/ci/os")
import oci_layers
labels = oci_layers.image_of_layout(sys.argv[1])[1].get("config", {}).get("Labels") or {}
print(labels.get("org.tilo.source-date-epoch", ""))
PY
) || die "could not read the clock of the layers below"
  [[ -z $epoch ]] && return 0
  [[ $epoch =~ ^[1-9][0-9]*$ ]] || die "the layers below carry no clock but '$epoch'"
  if [[ $epoch != "$SOURCE_DATE_EPOCH" ]]; then
    printf 'dated %s as the layers below it (SOURCE_DATE_EPOCH=%s), not %s from this run'"'"'s snapshot\n' \
      "$name" "$epoch" "$SOURCE_DATE_EPOCH"
  fi
  export SOURCE_DATE_EPOCH=$epoch
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
  reads_rpms "$name" || rpms_read=0
  build_env
  if board_layer "$name"; then board_env; fi
  phase load-given
  # What the piece is given as images: other actions' images, and compositions of layers.
  for entry in $(layer_field "$name" given); do
    case $entry in
      compiled)
        export TILO_COMPILED=$os/$compiled
        # A compile that keeps its build directory leaves the browser's engine to its own
        # check (os-chromium-ARCH), which this action needs then.
        if [[ ! -d $TILO_COMPILED/chromium-engine && -d $os/chromium/chromium-engine ]]; then
          export TILO_CHROMIUM_ENGINE=$os/chromium
        fi
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
  phase load-below
  local below=-
  if [[ $(layer_field "$name" scratch) != true ]]; then
    parent=$(python3 -I tools/ci/os/pieces.py parent "$name")
    mapfile -t frags < <(fragments_below "$name")
    below=$scratch/below
    rm -rf "$below"
    python3 -I tools/ci/os/oci_layers.py view "$below" "${frags[@]}"
    id=$(load "$below/oci")
    if [[ $parent == '${BASE_REF}' ]]; then
      podman tag "$id" localhost/tilo-base:pinned
    else
      layer_args+=(--build-context "$parent=container-image://$id")
      given+=$parent,
      # Helper stages FROM ${BASE_REF} build on the base as its layout has it, not pulled
      # from the registry again: its layers are the bottom of what was just loaded.
      podman tag "$(load "$os/base/oci")" localhost/tilo-base:pinned
    fi
    layer_args+=(--build-arg BASE_REF=localhost/tilo-base:pinned)
    below=$below/oci
    below_dated
  else
    # The helper stages FROM ${BASE_REF} build on the base as its layout has it.
    id=$(load "$os/base/oci")
    podman tag "$id" localhost/tilo-base:pinned
    layer_args+=(--build-arg BASE_REF=localhost/tilo-base:pinned)
  fi
  image=localhost/tilo-layer:$name
  phase build
  build_target "$name" "$target" "$given"
  phase push
  built=$scratch/built
  python3 -I tools/ci/os/oci_layers.py export "$image" "$below" "$built"
  phase take
  # Its key, kept with its layers: the digest of every file its piece reads.
  inputs=$scratch/inputs.json
  mapfile -t globs < <(python3 -I tools/ci/os/pieces.py inputs "$name" --given "$given")
  python3 -I tools/ci/os/oci_layers.py inputs "${globs[@]}" > "$inputs"
  rm -rf "$out"
  python3 -I tools/ci/os/oci_layers.py take "$below" "$built" "$out" "$name" "$inputs"
  rm -rf "$built"
  # Stages the actions above read beside the layers (the binaries' packing for the derived
  # state), from the same build's cache.
  phase also
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
flavour=${TILO_OS_FLAVOUR:-dev}
case $flavour in
  dev) compiled=compiled ;;
  release) compiled=compiled-release ;;
  *) die "TILO_OS_FLAVOUR is dev or release, not $flavour" ;;
esac
board=${TILO_OS_BOARD:-}
if [[ -n $board ]]; then
  [[ $flavour == release ]] || die "a board's image is built only for its release: TILO_OS_FLAVOUR=release"
  python3 -I -c 'import tomllib,sys; sys.exit(sys.argv[2] not in tomllib.load(open(sys.argv[1],"rb"))["release_boards"])' \
    os/image/layers.toml "$board" || die "$board is not among release_boards in os/image/layers.toml"
fi
case ${1:-} in
  base) arch=${2:?} out=${3:?} os=${TILO_OS_ROOT:-/target/os}/$arch; cmd_base ;;
  image) arch=${3:?} out=${4:?} os=${TILO_OS_ROOT:-/target/os}/$arch; cmd_image "${2:?}" ;;
  layer) arch=${3:?} out=${4:?} os=${TILO_OS_ROOT:-/target/os}/$arch; [[ $(uname -m) == "$arch" ]] || die "the $arch layers build natively"; cmd_layer "${2:?}" ;;
  compose) arch=${2:?} out=${3:?} os=${TILO_OS_ROOT:-/target/os}/$arch; cmd_compose ;;
  *) die "usage: build-layer.sh base ARCH OUT | image NAME ARCH OUT | layer NAME ARCH OUT | compose ARCH OUT" ;;
esac
