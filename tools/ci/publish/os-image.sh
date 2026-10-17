#!/usr/bin/env bash
# The OS image (os/Containerfile) on t, and the development images built on it, each as root in
# a micro-VM step. Nothing here holds a secret or publishes: os-board-publish.sh pushes a board
# image from a trusted run on main.
#
#   os-image.sh build ARCH OUT     build the image natively for ARCH into OUT/image.oci.tar, with
#                                  OUT/digest and OUT/revision
#   os-image.sh dev ARCH IN OUT    the dev image (os/Containerfile.dev: tilo-vmd, sshd, the toolchain,
#                                  the dev-overrides components) FROM the image `build` left in IN,
#                                  into OUT as `build` writes it
#   os-image.sh board BOARD IN OUT the board's sealed dev image (os/Containerfile.board: firmware,
#                                  device tree, initramfs, a UKI) FROM the aarch64 dev image `dev`
#                                  left in IN, into OUT as `build` writes it, with OUT/board
#
# A development build compiles the apps and the desktop of its own commit (no published
# collection or payload), without LTO, against the frozen RPM snapshot, the pinned base and the
# signed kernel, all of which os/dev/build-image.sh resolves and verifies itself. It stops at
# the container (TILO_IMAGE_ONLY=1): its UKI seals a placeholder composefs digest, so it is no
# install source; dev disks and releases reseal it after installing. It holds no secrets: apps
# it compiles are signed with a throwaway publisher, which no trust list names.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

# The RFC 8032 test-vector key: public and non-production, accepted only for development and
# validation images, so the owner-key plumbing runs before the production key is configured.
dev_owner_key=d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a

# Root's tools in the guest: podman and what build-image.sh verifies its inputs with.
guest_tools() {
  dnf install -y --setopt=install_weak_deps=False podman skopeo fuse-overlayfs e2fsprogs \
    util-linux jq python3 >/dev/null
  modprobe fuse 2>/dev/null || true
  modprobe loop 2>/dev/null || true
  if [[ ! -c /dev/fuse ]]; then
    mount -t devtmpfs devtmpfs /dev
    mkdir -p /dev/pts /dev/shm
    [[ $(stat -f -c %T /dev/pts) == devpts ]] || mount -t devpts -o gid=5,mode=620,ptmxmode=666 devpts /dev/pts
    mountpoint -q /dev/shm || mount -t tmpfs -o mode=1777 tmpfs /dev/shm
  fi
  use_cosign
  # /var/lib/containers is the step's own scratch disk: native overlay, no loop volume needed.
  podman info --format '{{.Store.GraphDriverName}} {{.Store.GraphRoot}}'
}

# An image `build` or `dev` left in DIR, into podman's store as NAME, after checking it is of
# this commit.
load_image() {
  local dir=$1 name=$2 revision=$3 id
  [[ $(cat "$dir/revision") == "$revision" ]] \
    || die "$dir/image.oci.tar is of $(cat "$dir/revision"), this build of $revision"
  [[ $(archive_digest "$dir/image.oci.tar") == "$(cat "$dir/digest")" ]] || die "$dir/image.oci.tar is not the build"
  id=$(podman pull -q "oci-archive:$dir/image.oci.tar")
  podman tag "$id" "$name"
}

# NAME from podman's store into OUT/image.oci.tar, with OUT/digest and OUT/revision.
save_image() {
  local name=$1 out=$2 revision=$3
  mkdir -p "$out"
  rm -f "$out/image.oci.tar"
  podman push --compression-format zstd --compression-level 3 "$name" "oci-archive:$out/image.oci.tar"
  archive_digest "$out/image.oci.tar" > "$out/digest"
  printf '%s\n' "$revision" > "$out/revision"
  printf 'built %s of %s\n' "$(cat "$out/digest")" "$revision"
}

# A value of os/publication.env, the public configuration of OS publication.
publication() {
  local value
  value=$(sed -n "s/^$1=//p" "$repo_root/os/publication.env")
  [[ -n $value ]] || die "os/publication.env sets no $1"
  printf '%s' "$value"
}

# The board's firmware, into os/hardware/firmware/BOARD (writable_sources), where
# os/Containerfile.board takes it from: the Lenovo's from its pinned artifact, the Orange Pi's
# Wi-Fi files from their pinned public copies.
stage_board_firmware() {
  case $1 in
    lenovo-83ed)
      python3 os/dev/hardware-firmware-artifact.py import \
        --reference "$(publication TILO_LENOVO_FIRMWARE_REF)" \
        --manifest-sha256 "$(publication TILO_LENOVO_FIRMWARE_MANIFEST_SHA256)" \
        --profile lenovo-83ed --architecture aarch64 --repo "$repo_root" >/dev/null ;;
    orangepi-3b-v2-1)
      TILO_GITHUB_ASSET_CACHE=$scratch/github-assets bash os/dev/fetch-orangepi-wifi-firmware.sh ;;
    *) die "no firmware source for the board $1" ;;
  esac
}

# The resealing bootc the board image computes its composefs digest with
# (os/Containerfile.bootc-helper): the published build of these sources, else built here.
bootc_helper() {
  local key ref
  key=$(cat os/Containerfile.bootc-helper os/external-assets/bootc-install-reseal/*.patch | sha256sum | cut -c1-16)
  ref=registry.tilo.so/tilo/bootc-install-helper:$key-$(uname -m)
  if ! podman pull -q "$ref" >/dev/null 2>&1; then
    printf 'no published %s; building it\n' "$ref" >&2
    podman build -q -f os/Containerfile.bootc-helper -t "$ref" . >/dev/null
  fi
  printf '%s' "$ref"
}

# os/Containerfile.board on the dev image BASE, sealed, as podman's localhost/tilo-board:ci. The
# UKI seals the composefs digest `bootc install` computes from the image's layers, which a build
# can't compute from its own merged tree (os/Containerfile.board): the first build seals a
# placeholder; the digest is computed from its layers, written out as they will be pushed; the
# second build changes only the UKI, its rootfs layers are the first's from the layer cache.
build_board() {
  local board=$1 base=$2 image=$3 work=$scratch/board-$1 tool_base helper cfs
  tool_base=$(. os/base-snapshot.env && printf '%s' "${BASE_REF:-}")
  [[ $tool_base =~ ^registry\.tilo\.so/tilo/fedora-bootc@sha256:[0-9a-f]{64}$ ]] \
    || die 'os/base-snapshot.env must pin the Fedora tooling base by digest'
  helper=$(bootc_helper)
  rm -rf "$work"
  mkdir -p "$work/cfs-source" "$work/cfs"
  local common=(--jobs 2 --security-opt label=disable -f os/Containerfile.board
    --build-context "board-firmware=os/hardware/firmware/$board"
    --build-arg "BASE_IMAGE=$base" --build-arg "BASE_REF=$tool_base" --build-arg "RPMS_REF=$RPMS_REF"
    --build-arg "TILO_HARDWARE_PROFILE=$board" --build-arg "TILO_TARGET_IMGREF=$image"
    --label "org.opencontainers.image.version=${image##*:}")
  podman build "${common[@]}" --build-arg "TILO_CFS_DIGEST=$(printf '0%.0s' $(seq 128))" \
    -t localhost/tilo-board:unsealed .
  podman push --compression-format zstd --compression-level 3 localhost/tilo-board:unsealed \
    "oci-archive:$work/cfs-source/image.oci.tar"
  podman build "${common[@]}" --target cfs-digest-out --build-arg "BOOTC_HELPER_IMAGE=$helper" \
    --build-context "cfs-source=$work/cfs-source" \
    --build-arg TILO_CFS_SOURCE=oci-archive:/run/tilo-cfs-source/image.oci.tar \
    --output "type=local,dest=$work/cfs" .
  cfs=$(tr -d '[:space:]' < "$work/cfs/digest")
  [[ $cfs =~ ^[0-9a-f]{128}$ ]] || die "$board: no composefs digest computed"
  printf '%s: composefs digest %s\n' "$board" "$cfs"
  podman build "${common[@]}" --build-arg "TILO_CFS_DIGEST=$cfs" -t localhost/tilo-board:ci .
  # Only the UKI may differ: the digest was computed from the first build's rootfs layers.
  local unsealed sealed
  unsealed=$(podman image inspect --format '{{json .RootFS.Layers}}' localhost/tilo-board:unsealed)
  sealed=$(podman image inspect --format '{{json .RootFS.Layers}}' localhost/tilo-board:ci)
  [[ ${sealed%,*} == "${unsealed%,*}" ]] || die "$board: the sealed build rebuilt rootfs layers the digest was computed from"
  rm -rf "$work"
}

case ${1:-} in
  build)
    arch=${2:?usage: os-image.sh build ARCH OUT}
    out=${3:?}
    [[ $(uname -m) == "$arch" ]] || die "the $arch image builds natively, not on $(uname -m)"
    guest_tools
    revision=$(git rev-parse --verify HEAD) || die 'the build names its commit: git = true'
    . os/base-snapshot.env
    RPMS_REF=$(bash os/dev/resolve-rpm-snapshot.sh)
    export BASE_REF RPMS_REF
    # Layers are dated by the base and the RPM snapshot, not the commit, so builds of different
    # commits share every layer whose content is the same.
    SOURCE_DATE_EPOCH=$(bash os/dev/image-epoch.sh "$BASE_REF" "$RPMS_REF")
    # The next generation of the published series, as a release would seal it; an empty series
    # is allowed since this image is never published as one.
    TILO_GENERATION=$(TILO_ALLOW_EMPTY_SERIES=1 bash os/dev/next-generation.sh)
    export SOURCE_DATE_EPOCH TILO_GENERATION
    export TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$dev_owner_key
    export TILO_IMAGE=localhost/tilo:ci TILO_IMAGE_ONLY=1 TILO_WITH_BROWSER=1
    export TILO_RUST_LTO=off TILO_RUST_DEBUGINFO=0 TILO_REQUIRE_PCR_PREDICTION=0
    export TILO_OUTPUT=$scratch/os-output TMPDIR=/var/tmp
    # About 3 GiB a job: links of the large binaries (the shell's alone near 7 GiB) overlap.
    jobs=$(( $(awk '/^MemTotal:/ {print $2}' /proc/meminfo) / 1024 / 1024 / 3 ))
    (( jobs > $(nproc) )) && jobs=$(nproc)
    (( jobs >= 1 )) || jobs=1
    export TILO_CARGO_BUILD_JOBS=$jobs
    export TILO_BUILD_ARGS="--label org.opencontainers.image.revision=$revision --label org.opencontainers.image.created=$(date -u -d "@$(git log -1 --format=%ct)" +%Y-%m-%dT%H:%M:%SZ)"
    bash os/dev/build-image.sh
    mkdir -p "$out"
    rm -f "$out/image.oci.tar"
    podman push --compression-format zstd --compression-level 3 "$TILO_IMAGE" "oci-archive:$out/image.oci.tar"
    archive_digest "$out/image.oci.tar" > "$out/digest"
    printf '%s\n' "$revision" > "$out/revision"
    printf 'built %s of %s\n' "$(cat "$out/digest")" "$revision"
    ;;
  dev)
    arch=${2:?usage: os-image.sh dev ARCH IN OUT}
    in=${3:?}
    out=${4:?}
    [[ $(uname -m) == "$arch" ]] || die "the $arch dev image builds natively, not on $(uname -m)"
    guest_tools
    revision=$(git rev-parse --verify HEAD) || die 'the build names its commit: git = true'
    load_image "$in" localhost/tilo:ci "$revision"
    # About 3 GiB a job, as `build`: the shell's dev build links the same large binaries.
    jobs=$(( $(awk '/^MemTotal:/ {print $2}' /proc/meminfo) / 1024 / 1024 / 3 ))
    (( jobs > $(nproc) )) && jobs=$(nproc)
    (( jobs >= 1 )) || jobs=1
    # tilo-vm moves a machine only onto a dev image whose base is no older than the one it
    # runs; the OS image's own clock is its base's and RPM snapshot's, so the commit's time
    # orders dev bases, as it did in Build / OS dev.
    TILO_DEV_BASE=localhost/tilo:ci TILO_DEV_IMAGE=localhost/tilo-vm:ci TILO_DEV_DISK=0 \
      TILO_DEV_BASE_CREATED=$(date -u -d "@$(git log -1 --format=%ct)" +%Y-%m-%dT%H:%M:%SZ) \
      TILO_CARGO_FRESHNESS=clean TILO_DEV_RUST_LTO=off TILO_DEV_CARGO_BUILD_JOBS=$jobs \
      TILO_DISTROBOX= TILO_OUTPUT=$scratch/dev-output TMPDIR=/var/tmp \
      TILO_BUILD_ARGS="--label org.opencontainers.image.revision=$revision" \
      bash os/dev/build-dev-image.sh
    save_image localhost/tilo-vm:ci "$out" "$revision"
    ;;
  board)
    board=${2:?usage: os-image.sh board BOARD IN OUT}
    in=${3:?}
    out=${4:?}
    profile=os/hardware/profiles/$board.json
    [[ -f $profile ]] || die "no hardware profile $profile"
    arch=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["architecture"])' "$profile")
    [[ $(uname -m) == "$arch" ]] || die "the $board image builds natively on $arch, not on $(uname -m)"
    guest_tools
    revision=$(git rev-parse --verify HEAD) || die 'the build names its commit: git = true'
    load_image "$in" localhost/tilo-vm:ci "$revision"
    stage_board_firmware "$board"
    # The snapshot the OS image was built from, as build-dev-image.sh takes it.
    RPMS_REF=$(podman image inspect --format '{{index .Labels "org.tilo.rpms.ref"}}' localhost/tilo-vm:ci)
    [[ $RPMS_REF =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] \
      || die "the dev image names no RPM snapshot (org.tilo.rpms.ref): $RPMS_REF"
    # The board image's update origin: the tag os-board-publish.sh pushes it under.
    build_board "$board" localhost/tilo-vm:ci "registry.tilo.so/tilo/tilo-vm:main-${revision:0:7}-$board"
    save_image localhost/tilo-board:ci "$out" "$revision"
    printf '%s\n' "$board" > "$out/board"
    ;;
  *) die 'usage: os-image.sh build ARCH OUT | dev ARCH IN OUT | board BOARD IN OUT' ;;
esac
