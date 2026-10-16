#!/usr/bin/env bash
# The OS image (os/Containerfile) on t: the development build every push to main publishes
# unsigned to the development-only repository tilo-ci, which production policy rejects
# (os/etc/containers/policy.json) and `tilo-vm vm upgrade --from sha:<commit>` finds.
#
#   os-image.sh build ARCH OUT   build the image natively for ARCH into OUT/image.oci.tar, with
#                                OUT/digest and OUT/revision (as root in a micro-VM step)
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
  *) die 'usage: os-image.sh build ARCH OUT' ;;
esac
