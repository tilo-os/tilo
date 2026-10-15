#!/usr/bin/env bash
# The Linux legacy VM guest disk (os/Containerfile.legacy-guest): Fedora bootc with Flatpak, the
# waypipe forwarder and Tilo's vsock guest agent, installed to a qcow2, booted once until the
# agent serves, and published as a single-layer image whose digest os/legacy-guest-snapshot.env
# pins; `tilo-legacy` fetches only that digest.
#
#   legacy-guest.sh agent OUT           build the guest agent into OUT (CI builder)
#   legacy-guest.sh build AGENT OUT     build the guest image with that agent, install it to a
#                                       disk, boot it until the agent serves, and wrap the disk
#                                       into OUT/image.oci.tar (a privileged x86_64 node, as root)
#   legacy-guest.sh binaries OUT        the shell under test (debug, agent-control), tilo-legacy
#                                       and the relay's integration tests (CI builder)
#   legacy-guest.sh e2e GUEST BINARIES IMAGE
#                                       boot GUEST's disk through tilo-legacy against a headless
#                                       shell (os/dev/legacy-vm-e2e.sh) in the CI builder IMAGE,
#                                       privileged with KVM (a privileged x86_64 node, as root)
#   legacy-guest.sh release OUT         publish the signed OUT and write OUT/pins.env (skopeo)
#   legacy-guest.sh pin                 write os/legacy-guest-snapshot.env from stable's pins
#                                       (T_POINTER_LEGACY_GUEST) when main builds the same guest
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/legacy-guest
pins=os/legacy-guest-snapshot.env

# Everything that ends up in the guest disk (os/dev/legacy-guest-snapshot.sh's INPUTS).
inputs_digest() {
  find os/Containerfile.legacy-guest os/legacy-guest os/base-snapshot.env \
    crates/runtime/legacy-agent crates/runtime/legacy-agent-protocol -type f -print0 \
    | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1
}

case ${1:-} in
  agent)
    out=${2:?usage: legacy-guest.sh agent OUT}
    cargo build --locked --release -p legacy-agent --bin tilo-legacy-agent
    install -D -m 0755 "${CARGO_TARGET_DIR:-target}/release/tilo-legacy-agent" "$out/tilo-legacy-agent"
    ;;
  build)
    agent=${2:?usage: legacy-guest.sh build AGENT OUT}
    out=${3:?}
    [[ $(uname -m) == x86_64 ]] || die 'the legacy guest is built on x86_64'
    dnf install -y --setopt=install_weak_deps=False btrfs-progs dosfstools e2fsprogs edk2-ovmf \
      fuse-overlayfs jq python3 qemu-img qemu-system-x86-core skopeo systemd-repart util-linux >/dev/null
    # The VM's /dev is static; the kernel's own loop and FUSE nodes appear with devtmpfs.
    modprobe fuse 2>/dev/null || true
    modprobe loop 2>/dev/null || true
    modprobe vhost_vsock 2>/dev/null || true
    if [[ ! -c /dev/loop-control || ! -c /dev/fuse ]]; then
      mount -t devtmpfs devtmpfs /dev
      mkdir -p /dev/pts /dev/shm
      [[ $(stat -f -c %T /dev/pts) == devpts ]] || mount -t devpts -o gid=5,mode=620,ptmxmode=666 devpts /dev/pts
      mountpoint -q /dev/shm || mount -t tmpfs -o mode=1777 tmpfs /dev/shm
    fi
    losetup --find >/dev/null
    python3 os/dev/probe-kvm.py x86_64 || die 'no KVM: an unbooted guest is never published'
    RUNNER_TEMP=$scratch CONTAINERS_STORAGE_CONF=$(RUNNER_TEMP=$scratch bash os/dev/ci-native-podman-store.sh)
    export RUNNER_TEMP CONTAINERS_STORAGE_CONF
    use_cosign
    . os/base-snapshot.env
    install -D -m 0755 "$agent/tilo-legacy-agent" os/legacy-guest/agent/tilo-legacy-agent
    guest=localhost/tilo-legacy-guest:ci
    podman build --pull=always -f os/Containerfile.legacy-guest --build-arg "BASE_REF=$BASE_REF" -t "$guest" .
    disk=$scratch/legacy-guest-disk
    mkdir -p "$disk"
    TILO_QCOW2_COMPRESS=1 bash os/dev/disk-image.sh "$guest" "$disk" --bootloader grub --rootfs ext4 --size 16G
    test -s "$disk/disk.qcow2"
    bash os/dev/legacy-guest-smoke.sh "$disk/disk.qcow2"
    # One layer holding the disk, fixed timestamps: the digest is the disk's.
    printf 'FROM scratch\nCOPY disk.qcow2 /disk.qcow2\n' > "$disk/Containerfile"
    podman build --timestamp 0 -f "$disk/Containerfile" -t localhost/legacy-guest-disk "$disk"
    mkdir -p "$out"
    rm -f "$out/image.oci.tar"
    podman push localhost/legacy-guest-disk "oci-archive:$out/image.oci.tar"
    archive_digest "$out/image.oci.tar" > "$out/digest"
    printf 'built %s\n' "$(cat "$out/digest")"
    ;;
  binaries)
    out=${2:?usage: legacy-guest.sh binaries OUT}
    target=${CARGO_TARGET_DIR:-target}
    # Debug: the shell's test-control socket exists only in debug builds.
    CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked -p shell --bin shell --features agent-control \
      -p legacy-vm --bin tilo-legacy
    install -D -m 0755 "$target/debug/shell" "$out/shell"
    install -D -m 0755 "$target/debug/tilo-legacy" "$out/tilo-legacy"
    relay_tests=$(cargo test --locked -p tilo-relay --test it --no-run --message-format=json \
      | jq -r 'select(.executable != null and .target.name == "it") | .executable' | tail -n 1)
    test -x "$relay_tests"
    install -D -m 0755 "$relay_tests" "$out/relay-it"
    ;;
  e2e)
    guest=${2:?usage: legacy-guest.sh e2e GUEST BINARIES IMAGE}
    binaries=${3:?}
    image=${4:?}
    modprobe vhost_vsock 2>/dev/null || echo 'no vhost_vsock: the test uses vhost-device-vsock'
    test -c /dev/kvm
    work=$scratch/legacy-e2e
    rm -rf "$work"
    mkdir -p "$work/e2e/disk" "$work/e2e/bin" "$work/e2e/artifacts"
    # The disk is the single layer of the wrapped image.
    id=$(podman pull --quiet "oci-archive:$guest/image.oci.tar")
    container=$(podman create "$id" /disk.qcow2)
    podman cp "$container:/disk.qcow2" "$work/e2e/disk/disk.qcow2"
    podman rm "$container" >/dev/null
    cp "$binaries"/{shell,tilo-legacy,relay-it} "$work/e2e/bin/"
    cp -a "$repo_root/." "$work/src"
    mv "$work/e2e" "$work/src/e2e"
    chmod -R a+rwX "$work/src/e2e"
    devices=(--device /dev/kvm)
    [[ ! -c /dev/vhost-vsock ]] || devices+=(--device /dev/vhost-vsock)
    podman run --rm --privileged --network host "${devices[@]}" -v "$work/src:/w" -w /w "$image" \
      bash os/dev/legacy-vm-e2e.sh
    ;;
  release)
    out=${2:?usage: legacy-guest.sh release OUT}
    bash tools/ci/publish/oci-publish.sh release "$repository" "$out" 'x86_64-{commit}'
    printf 'LEGACY_GUEST_X86_64_REF=%s@%s\nINPUTS_SHA256=%s\n' \
      "$repository" "$(cat "$out/digest")" "$(inputs_digest)" > "$out/pins.env"
    cat "$out/pins.env"
    ;;
  pin)
    [[ -n ${T_POINTER_LEGACY_GUEST:-} ]] || { echo 'stable has published no legacy guest yet'; exit 0; }
    ref=$(sed -n 's/^LEGACY_GUEST_X86_64_REF=//p' <<<"$T_POINTER_LEGACY_GUEST")
    built_from=$(sed -n 's/^INPUTS_SHA256=//p' <<<"$T_POINTER_LEGACY_GUEST")
    if [[ $built_from != "$(inputs_digest)" ]]; then
      echo "stable's legacy guest was built from other inputs than main's; leaving the pin"
      exit 0
    fi
    [[ $ref =~ ^registry\.tilo\.so/tilo/legacy-guest@sha256:[0-9a-f]{64}$ ]] \
      || die "not an immutable legacy guest reference: $ref"
    use_cosign
    cosign verify --key keys/cosign.pub "$ref" >/dev/null
    python3 os/dev/open-snapshot-pr.py pins legacy-guest --x86-ref "$ref" > "$pins"
    printf 'build(os): pin the Linux legacy VM guest image\n' > .t-commit-message
    cat "$pins"
    ;;
  *) die 'usage: legacy-guest.sh agent|build|release|pin ...' ;;
esac
