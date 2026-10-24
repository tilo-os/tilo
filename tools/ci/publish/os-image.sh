#!/usr/bin/env bash
# Images built on the OS image, each as root in a micro-VM step. The OS image itself is
# composed from layer actions (os/image/layers.toml, os/image/build-layer.sh) and comes here
# as the OCI layout os-image-<arch> left. Nothing here holds a secret or publishes:
# os-board-publish.sh pushes a board image from a trusted run on main.
#
#   os-image.sh dev ARCH IN OUT COMPILED
#                                  the dev image (os/Containerfile.dev: tilo-vmd, sshd, the toolchain,
#                                  the dev-overrides components compiled in COMPILED by
#                                  os/image/compile.sh --dev) FROM the OS image in IN (IN/oci), as
#                                  an OCI layout in OUT/oci, its layers over the OS image's
#                                  composed with them, with OUT/digest and OUT/revision
#   os-image.sh board BOARD IN OUT the board's sealed dev image (os/Containerfile.board: firmware,
#                                  device tree, initramfs, a UKI) FROM the aarch64 dev image `dev`
#                                  left in IN, into OUT/image.oci.tar, with OUT/digest, OUT/revision
#                                  and OUT/board; the Lenovo's audio stack checked inside it; and
#                                  its flashable disk in OUT/disk
#
# They run in the OS assembler image (os/image/assembler.Containerfile), which has podman and
# the tools build-image.sh verifies its inputs with; a guest without them installs them first.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

# Root's tools in the guest: podman and what build-image.sh verifies its inputs with.
guest_tools() {
  command -v podman >/dev/null && command -v skopeo >/dev/null && command -v jq >/dev/null \
    || dnf install -y --setopt=install_weak_deps=False podman skopeo fuse-overlayfs e2fsprogs \
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

# An image in DIR into podman's store as NAME: the OS image's layout (DIR/oci, composed from
# layers, named by its digest whatever commit's run composed it), or an archive `dev` left,
# checked to be of this commit.
load_image() {
  local dir=$1 name=$2 revision=$3 id
  if [[ -d $dir/oci ]]; then
    id=$(podman pull -q "oci:$dir/oci")
    [[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$dir/digest")" ]] || die "$dir/oci is not the image $dir/digest names"
  else
    [[ $(cat "$dir/revision") == "$revision" ]] \
      || die "$dir/image.oci.tar is of $(cat "$dir/revision"), this build of $revision"
    [[ $(archive_digest "$dir/image.oci.tar") == "$(cat "$dir/digest")" ]] || die "$dir/image.oci.tar is not the build"
    id=$(podman pull -q "oci-archive:$dir/image.oci.tar")
  fi
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
  phase board-cfs-source
  podman push --compression-format zstd --compression-level 3 localhost/tilo-board:unsealed \
    "oci-archive:$work/cfs-source/image.oci.tar"
  phase board-cfs-digest
  podman build "${common[@]}" --target cfs-digest-out --build-arg "BOOTC_HELPER_IMAGE=$helper" \
    --build-context "cfs-source=$work/cfs-source" \
    --build-arg TILO_CFS_SOURCE=oci-archive:/run/tilo-cfs-source/image.oci.tar \
    --output "type=local,dest=$work/cfs" .
  cfs=$(tr -d '[:space:]' < "$work/cfs/digest")
  [[ $cfs =~ ^[0-9a-f]{128}$ ]] || die "$board: no composefs digest computed"
  printf '%s: composefs digest %s\n' "$board" "$cfs"
  phase board-sealed
  podman build "${common[@]}" --build-arg "TILO_CFS_DIGEST=$cfs" -t localhost/tilo-board:ci .
  # Only the UKI may differ: the digest was computed from the first build's rootfs layers.
  local unsealed sealed
  unsealed=$(podman image inspect --format '{{json .RootFS.Layers}}' localhost/tilo-board:unsealed)
  sealed=$(podman image inspect --format '{{json .RootFS.Layers}}' localhost/tilo-board:ci)
  [[ ${sealed%,*} == "${unsealed%,*}" ]] || die "$board: the sealed build rebuilt rootfs layers the digest was computed from"
  rm -rf "$work"
}

# The frozen RPM snapshot the build installs from, as RPMS_REF: the published one for this
# tree's package set, verified as every build verifies it. Only stable publishes snapshots, so
# on main and branches a changed package set has none until it reaches stable; the build then
# installs from the set image-rpms-ARCH built in this run from the same os/Containerfile.rpms,
# which build-image.sh takes unsigned (TILO_RPMS_TRUST=unsigned) and labels so
# (org.tilo.rpms.trust), and which nothing signs or publishes.
rpms_snapshot() {
  local arch=$1 built=/target/publish/rpms/$1 status=0 id
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
      printf 'no published RPM snapshot for this package set: installing from image-rpms-%s (%s), unsigned\n' \
        "$arch" "$(cat "$built/digest")" ;;
    *) die 'could not ask the registry for the published RPM snapshot' ;;
  esac
}

# The flashable disk of the sealed board image NAME, into DIR/disk.qcow2, never
# booted: installed as build-image.sh installs a dev disk, following tilo-vm:BOARD-ci, which
# os-board-publish.sh moves. The Orange Pi's disk carries the loader its mask ROM reads in front
# of the ESP (build-image.sh builds it with os/dev/build-orangepi-uboot.sh) and is 16 GB, for an
# SD card (its root grows on first boot); the Lenovo's 64 GB.
install_board_disk() {
  local board=$1 name=$2 dir=$3 id size=64G loader=
  id=$(podman image inspect --format '{{.Id}}' "$name")
  if [[ $board == orangepi-3b-v2-1 ]]; then
    size=16G loader=$scratch/rockchip-loader
    rpm -q bc bison flex gcc make dtc swig python3-devel python3-pyelftools python3-setuptools \
      gnutls-devel openssl-devel-engine >/dev/null 2>&1 \
      || dnf install -y --setopt=install_weak_deps=False bc bison flex gcc make dtc swig python3-devel \
        python3-pyelftools python3-setuptools gnutls-devel openssl-devel-engine >/dev/null
  fi
  rpm -q dosfstools btrfs-progs systemd-container binutils qemu-img >/dev/null 2>&1 \
    || dnf install -y --setopt=install_weak_deps=False dosfstools btrfs-progs systemd-container binutils qemu-img >/dev/null
  rm -rf "$dir"
  mkdir -p "$dir"
  TILO_IMAGE=$name TILO_SKIP_BUILD=1 TILO_DISTROBOX= TILO_OUTPUT=$scratch/board-disk \
    TILO_RUN=$scratch/board-disk-run TILO_TARGET_IMGREF=registry.tilo.so/tilo/tilo-vm:$board-ci \
    TILO_DISK_SIZE=$size TILO_ROCKCHIP_LOADER=$loader TILO_QCOW2_COMPRESS=1 \
    TMPDIR=/var/tmp bash os/dev/build-image.sh
  # The image was sealed with the digest install computes: an install that resealed it would
  # leave a disk of another image than the one published.
  [[ $(podman image inspect --format '{{.Id}}' "$name") == "$id" ]] \
    || die "$board: install computed another composefs digest than the build sealed"
  mv "$scratch/board-disk/qcow2/disk.qcow2" "$dir/disk.qcow2"
}

# How long each part took, for finding what a build spends its time on.
phase_started=$SECONDS phase_name=start
phase() {
  printf 'TILO_IMAGE_TIMING phase=%s seconds=%s\n' "$phase_name" "$((SECONDS - phase_started))"
  phase_name=$1 phase_started=$SECONDS
}
trap 'phase end' EXIT

case ${1:-} in
  dev)
    arch=${2:?usage: os-image.sh dev ARCH IN OUT}
    in=${3:?}
    out=${4:?}
    [[ $(uname -m) == "$arch" ]] || die "the $arch dev image builds natively, not on $(uname -m)"
    guest_tools
    revision=$(git rev-parse --verify HEAD) || die 'the build names its commit: git = true'
    compiled=${5:?usage: os-image.sh dev ARCH IN OUT COMPILED}
    phase load
    load_image "$in" localhost/tilo:ci "$revision"
    # The snapshot the OS image was built from, which build-dev-image.sh would otherwise pull by
    # the image's label: a local set from image-rpms-ARCH exists only in the job that made it.
    rpms_snapshot "$arch"
    export RPMS_REF
    # tilo-vm moves a machine only onto a dev image whose base is no older than the one it
    # runs; the OS image's own clock is its base's and RPM snapshot's, so the commit's time
    # orders dev bases, as it did in Build / OS dev. The dev components come compiled
    # (os-compile-dev-<arch>): nothing compiles here.
    phase build
    TILO_DEV_BASE=localhost/tilo:ci TILO_DEV_IMAGE=localhost/tilo-vm:ci TILO_DEV_DISK=0 \
      TILO_DEV_BASE_CREATED=$(date -u -d "@$(git log -1 --format=%ct)" +%Y-%m-%dT%H:%M:%SZ) \
      TILO_COMPILED_DEV=$compiled \
      TILO_DISTROBOX= TILO_OUTPUT=$scratch/dev-output TMPDIR=/var/tmp \
      bash os/dev/build-dev-image.sh
    # Kept as the layers it added over the OS image's, composed with them: the OS image's
    # layers are the same blobs in both, stored and sent once.
    phase push
    dev=$scratch/dev-built
    rm -rf "$dev" "$out"
    podman push -q --compression-format gzip --force-compression localhost/tilo-vm:ci "oci:$dev:tilo"
    printf '{"inputs":{},"inputsDigest":""}' > "$scratch/dev-inputs.json"
    python3 -I tools/ci/os/oci_layers.py take "$in/oci" "$dev" "$scratch/dev-fragment" dev-image "$scratch/dev-inputs.json"
    python3 -I tools/ci/os/oci_layers.py compose "$out" "$in/oci" "$scratch/dev-fragment"
    printf '%s\n' "$revision" > "$out/revision"
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
    phase load
    load_image "$in" localhost/tilo-vm:ci "$revision"
    phase firmware
    stage_board_firmware "$board"
    # The snapshot the OS image was built from, as the dev image takes it.
    rpms_snapshot "$arch"
    # The board image's update origin: the tag os-board-publish.sh pushes it under.
    phase build
    build_board "$board" localhost/tilo-vm:ci "registry.tilo.so/tilo/tilo-vm:main-${revision:0:7}-$board"
    phase save
    save_image localhost/tilo-board:ci "$out" "$revision"
    printf '%s\n' "$board" > "$out/board"
    if [[ $board == lenovo-83ed ]]; then
      # Its sound card needs the topology, DSP firmware and UCM profile in the image; booting
      # it on the laptop is the only test of the sound itself.
      phase audio
      podman run --rm --network none --entrypoint bash \
        -v "$repo_root/os/tests/board_audio_contract.sh:/run/board_audio_contract.sh:ro" \
        localhost/tilo-board:ci /run/board_audio_contract.sh --root /
    fi
    phase disk
    install_board_disk "$board" localhost/tilo-board:ci "$out/disk"
    ;;
  *) die 'usage: os-image.sh dev ARCH IN OUT COMPILED | board BOARD IN OUT' ;;
esac
