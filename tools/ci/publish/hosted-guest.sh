#!/usr/bin/env bash
# The hosted workspace VM guest: an uncompressed Firecracker kernel and the raw
# ext4 root filesystem every hosted VM's disk starts as
# (os/Containerfile.hosted-guest, os/hosted-guest/), published as one
# single-layer OCI image whose digest os/hosted-guest-snapshot.env pins;
# `tilo-legacy hosted fetch-guest` fetches only that digest.
#
#   hosted-guest.sh kernel OUT          build vmlinux from the pinned kernel.org
#                                       source and this repository's configuration
#                                       into OUT, asserting every required option
#                                       against the resolved config (CI builder)
#   hosted-guest.sh agent OUT           build the guest agent into OUT — the legacy
#                                       VM's agent, built by the legacy VM's script
#   hosted-guest.sh build AGENT KERNEL OUT
#                                       export os/Containerfile.hosted-guest's
#                                       filesystem into rootfs.ext4 and wrap the
#                                       kernel and it into OUT/image.oci.tar (as root
#                                       in an x86_64 micro-VM step)
#   hosted-guest.sh pins OUT            after publish-hosted-guest published OUT,
#                                       write OUT/pins.env: its reference and the
#                                       inputs it was built from
#   hosted-guest.sh pin                 write os/hosted-guest-snapshot.env from
#                                       stable's pins (T_POINTER_HOSTED_GUEST) when
#                                       main builds the same guest
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/hosted-guest
pins=os/hosted-guest-snapshot.env
# A longterm kernel, pinned by URL and checksum in
# os/external-assets/github-assets.tsv like every other external input.
kernel_version=6.12.116
# The node, the version manager and the rust toolchain the guest ships. Their
# bytes come from the pinned assets, never from a network fetch during a build.
node_asset=node-v26.13.0-linux-x64.tar.xz
fnm_asset=fnm-1.39.0-linux.zip
rustup_init_asset=rustup-init-x86_64-unknown-linux-gnu
guest_assets=("linux-$kernel_version.tar.xz" "$node_asset" "$fnm_asset" "$rustup_init_asset")
# Room a micro-VM step's root needs to unpack a kernel tree and compile it.
kernel_build_packages=(bc bison diffutils dwarves elfutils-libelf-devel findutils flex gcc
  make openssl-devel perl-core python3 tar which xz)

# Everything that ends up in the guest image.
inputs_digest() {
  find os/Containerfile.hosted-guest os/hosted-guest os/external-assets/github-assets.tsv \
    crates/runtime/legacy-agent crates/runtime/legacy-agent-protocol -type f -print0 \
    | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1
}

# The resolved kernel configuration must be the one this repository asks for:
# a fragment that drifted from what a VM needs fails the build here rather than
# in somebody's workspace.
assert_kernel_config() {
  python3 "$repo_root/os/kernel/check-config.py" "$1" \
    --fragment "$repo_root/os/hosted-guest/kernel/fragment.config"
}

case ${1:-} in
  kernel)
    out=${2:?usage: hosted-guest.sh kernel OUT}
    [[ $(uname -m) == x86_64 ]] || die 'the hosted guest kernel is built for x86_64'
    assets=$(pinned_assets "${guest_assets[@]}")
    dnf install -y --setopt=install_weak_deps=False "${kernel_build_packages[@]}" >/dev/null
    work=$scratch/hosted-guest-kernel
    rm -rf "$work"
    mkdir -p "$work"
    tar -xJf "$assets/linux-$kernel_version.tar.xz" -C "$work"
    cd "$work/linux-$kernel_version"
    cp "$repo_root/os/hosted-guest/kernel/base.config" .config
    sh scripts/kconfig/merge_config.sh -m .config \
      "$repo_root/os/hosted-guest/kernel/fragment.config" >/dev/null
    make -s olddefconfig
    assert_kernel_config .config
    make -s -j"$(nproc)" vmlinux
    # Straight from the build: the hypervisor loads the ELF's segments, and a
    # kernel that boots is worth more than the tens of megabytes its symbols and
    # debug info would save.
    install -d "$out"
    install -m 0644 vmlinux "$out/vmlinux"
    make -s kernelrelease > "$out/kver"
    printf 'KERNEL_VERSION=%s\n' "$(cat "$out/kver")"
    du -h "$out/vmlinux"
    ;;
  agent)
    # The same agent binary as the legacy VM's, built by the same script: one
    # guest agent, one build, whichever hypervisor boots it.
    bash "$repo_root/tools/ci/publish/legacy-guest.sh" agent "${2:?usage: hosted-guest.sh agent OUT}"
    ;;
  build)
    agent=${2:?usage: hosted-guest.sh build AGENT KERNEL OUT}
    kernel=${3:?}
    out=${4:?}
    [[ $(uname -m) == x86_64 ]] || die 'the hosted guest is built on x86_64'
    dnf install -y --setopt=install_weak_deps=False e2fsprogs findutils jq podman tar util-linux >/dev/null
    # The VM's /dev is static; the loop devices the store below lives on appear
    # with devtmpfs.
    modprobe loop 2>/dev/null || true
    if [[ ! -c /dev/loop-control ]]; then
      mount -t devtmpfs devtmpfs /dev
    fi
    losetup --find >/dev/null
    RUNNER_TEMP=$scratch CONTAINERS_STORAGE_CONF=$(RUNNER_TEMP=$scratch bash os/dev/ci-native-podman-store.sh)
    export RUNNER_TEMP CONTAINERS_STORAGE_CONF
    assets=$(pinned_assets "${guest_assets[@]}")
    . os/hosted-guest/base.env
    guest=localhost/tilo-hosted-guest:ci
    podman build --pull=always -f os/Containerfile.hosted-guest . \
      --build-arg "BASE_REF=$HOSTED_GUEST_BASE_REF" \
      --build-arg "NODE_ASSET=$node_asset" \
      --build-arg "FNM_ASSET=$fnm_asset" \
      --build-arg "RUSTUP_INIT_ASSET=$rustup_init_asset" \
      --build-context "github-assets=$assets" \
      --build-context "hosted-agent=$agent" \
      -t "$guest"
    # The container's filesystem, then a filesystem image of it where it lands:
    # no loop device and no root filesystem of the host involved.
    root=$scratch/hosted-guest-root
    image=$scratch/hosted-guest-image
    rm -rf "$root" "$image"
    mkdir -p "$root" "$image" "$out"
    container=$(podman create "$guest" true)
    podman export "$container" | tar -x -C "$root" --numeric-owner
    podman rm "$container" >/dev/null
    test -x "$root/usr/libexec/tilo/hosted-init"
    test -x "$root/usr/libexec/tilo/tilo-legacy-agent"
    test -s "$root/home/tilo/.cargo/bin/rustup"
    # Just over what is in it: the host grows each VM's copy to the VM's own
    # size, and the guest grows the filesystem to that at boot. A tenth and 64
    # MiB of slack covers inode tables, the journal and a little growth.
    bytes=$(du -sb --apparent-size "$root" | cut -f1)
    mib=$(( (bytes + bytes / 10 + 64 * 1024 * 1024 + 1048575) / 1048576 ))
    rootfs=$image/rootfs.ext4
    truncate -s "${mib}M" "$rootfs"
    mke2fs -q -t ext4 -F -m 0 -L tilo -d "$root" "$rootfs"
    e2fsck -fn "$rootfs" >/dev/null
    install -m 0644 "$kernel/vmlinux" "$image/vmlinux"
    printf 'root image: %s MiB (apparent %s MiB), kernel %s\n' \
      "$mib" "$(( (bytes + 1048575) / 1048576 ))" "$(cat "$kernel/kver")"
    du -h "$image/vmlinux" "$rootfs"
    # One layer holding both, fixed timestamps: the digest is the guest's. The
    # two files are one COPY so that the image has one layer, not one per file.
    printf 'FROM scratch\nCOPY vmlinux rootfs.ext4 /\n' > "$image/Containerfile"
    podman build --timestamp 0 -f "$image/Containerfile" -t localhost/hosted-guest-image "$image"
    rm -f "$out/image.oci.tar"
    podman push localhost/hosted-guest-image "oci-archive:$out/image.oci.tar"
    archive_digest "$out/image.oci.tar" > "$out/digest"
    printf 'built %s\n' "$(cat "$out/digest")"
    ;;
  pins)
    out=${2:?usage: hosted-guest.sh pins OUT}
    [[ -s $out/receipt.json ]] || die "$out is not published"
    printf 'HOSTED_GUEST_X86_64_REF=%s@%s\nINPUTS_SHA256=%s\n' \
      "$repository" "$(cat "$out/digest")" "$(inputs_digest)" > "$out/pins.env"
    cat "$out/pins.env"
    ;;
  pin)
    [[ -n ${T_POINTER_HOSTED_GUEST:-} ]] || { echo 'stable has published no hosted guest yet'; exit 0; }
    ref=$(sed -n 's/^HOSTED_GUEST_X86_64_REF=//p' <<<"$T_POINTER_HOSTED_GUEST")
    built_from=$(sed -n 's/^INPUTS_SHA256=//p' <<<"$T_POINTER_HOSTED_GUEST")
    if [[ $built_from != "$(inputs_digest)" ]]; then
      echo "stable's hosted guest was built from other inputs than main's; leaving the pin"
      exit 0
    fi
    [[ $ref =~ ^registry\.tilo\.so/tilo/hosted-guest@sha256:[0-9a-f]{64}$ ]] \
      || die "not an immutable hosted guest reference: $ref"
    use_cosign
    cosign verify --key keys/cosign.pub "$ref" >/dev/null
    python3 os/dev/open-snapshot-pr.py pins hosted-guest --x86-ref "$ref" > "$pins"
    printf 'build(os): pin the hosted workspace VM guest image\n' > .t-commit-message
    cat "$pins"
    ;;
  *) die 'usage: hosted-guest.sh kernel|agent|build|pins|pin ...' ;;
esac
