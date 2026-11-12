#!/usr/bin/env bash
# The hosted VM guest: an uncompressed Firecracker kernel and the raw ext4
# root filesystem every workspace VM's disk starts as (os/Containerfile.hosted-guest),
# published as one single-layer OCI image whose digest
# os/hosted-guest-snapshot.env pins; `tilo-legacy hosted fetch-guest` fetches
# only that digest.
#
#   hosted-guest.sh kernel OUT          build vmlinux from the pinned kernel.org
#                                       source and this repository's configuration
#                                       into OUT, asserting every required option
#                                       against the resolved config (CI builder)
#   hosted-guest.sh agent OUT           build the guest agent into OUT — the legacy
#                                       VM's agent, built by the legacy VM's script
#   hosted-guest.sh session OUT         build the guest's Tilo session into OUT/usr:
#                                       the session, its surface apps, the launcher, the
#                                       store's tools and the fonts, placed by
#                                       os/components.toml
#   hosted-guest.sh build SESSION COMPILED CHROMIUM KERNEL AGENT FONTS OUT
#                                       export the guest's filesystem into rootfs.ext4,
#                                       over the session, the compiled tree
#                                       (os/image/compile.sh) and the engine
#                                       (os-chromium-<arch>), and wrap the kernel and it
#                                       into OUT/image.oci.tar (as root in an x86_64
#                                       micro-VM step)
#   hosted-guest.sh pins OUT            after publish-hosted-guest published OUT,
#                                       write OUT/pins.env: its reference and the
#                                       inputs it was built from
#   hosted-guest.sh pin                 write os/hosted-guest-snapshot.env's
#                                       HOSTED_GUEST_<ARCH>_REF from stable's pins
#                                       (T_POINTER_HOSTED_GUEST) when main builds the
#                                       same guest
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/hosted-guest
pins=os/hosted-guest-snapshot.env
# A longterm kernel, pinned by URL and checksum in
# os/external-assets/github-assets.tsv like every other external input.
kernel_version=6.12.116
# The node, the version manager and the rust toolchain the workspace guest
# ships. Their bytes come from the pinned assets, never from a network fetch
# during a build.
node_asset=node-v26.13.0-linux-x64.tar.xz
fnm_asset=fnm-1.39.0-linux.zip
rustup_init_asset=rustup-init-x86_64-unknown-linux-gnu
guest_assets=("linux-$kernel_version.tar.xz" "$node_asset" "$fnm_asset" "$rustup_init_asset")
# Room a micro-VM step's root needs to unpack a kernel tree and compile it.
kernel_build_packages=(bc bison diffutils dwarves elfutils-libelf-devel findutils flex gcc
  make openssl-devel perl-core python3 tar which xz)
# The components of the guest's session: os/components.toml is the one
# record of which Tilo-built file lands where, so the image names components and
# never a binary or a path. The launcher starts an app sandboxed and mounts its
# runtimes; the store's tools install and verify what the image ships.
session_components=(tilo-session control launcher install app-package runtime-bundle cli-shim)

# Everything that ends up in the guest image: its recipe, the agent, what
# arranges a session and an app store, the sources of the components it builds
# and the engine version its Browser requires.
inputs_digest() {
  find os/Containerfile.hosted-guest os/hosted-guest os/external-assets/github-assets.tsv \
    crates/runtime/legacy-agent crates/runtime/legacy-agent-protocol \
    crates/session crates/packaging os/components.toml \
    os/scripts/install-components.py os/scripts/pack-apps.sh \
    os/scripts/signed-records.py os/scripts/collect-runtime-dependencies.py \
    os/scripts/verify-runtime-bundle-closure.sh os/etc/containers/policy.json \
    apps/browser crates/sdk/ui-core/src/webview/chromium crates/sdk/ui-core/assets/fonts \
    tools/font-catalog.py tools/apps tools/perf/tapp-size-budget \
    tools/ci/publish/hosted-guest.sh -type f -print0 \
    | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1
}

# The engine runtime's version, which ui::webview::chromium::runtime.rs is the one
# name of: the image's Browser requires exactly the bundle the build makes.
chromium_runtime_version() {
  local version
  version=$(sed -n 's/^pub const RUNTIME_VERSION: &str = "\([^"]*\)";$/\1/p' \
    crates/sdk/ui-core/src/webview/chromium/runtime.rs)
  [[ -n $version ]] || die 'the Chromium runtime names no RUNTIME_VERSION'
  printf '%s' "$version"
}

# The resolved kernel configuration must be the one this repository asks for:
# a fragment that drifted from what a VM needs fails the build here rather than
# in somebody's workspace.
assert_kernel_config() {
  python3 "$repo_root/os/kernel/check-config.py" "$1" \
    --fragment "$repo_root/os/hosted-guest/kernel/fragment.config"
}

# What a container build of a guest needs: e2fsprogs and podman, a loop-capable
# /dev, and a container store under the step's own scratch.
container_build_setup() {
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
}

# The container's filesystem where it lands: a root tree to make a filesystem
# image of, with no loop device and no root filesystem of the host involved.
export_rootfs() {
  local guest=$1 root=$2
  rm -rf "$root"
  mkdir -p "$root"
  podman export "$(podman create "$guest" true)" | tar -x -C "$root" --numeric-owner
}

# The guest itself: the root tree as a raw ext4, the kernel beside it, and the
# two wrapped in one OCI image whose digest is the guest's.
#
# Just over what is in it: the host grows each VM's copy to the VM's own size,
# and the guest grows the filesystem to that at boot. A tenth and 64 MiB of
# slack covers inode tables, the journal and a little growth.
wrap_guest() {
  local root=$1 kernel=$2 out=$3 name=$4 image bytes mib rootfs
  image=$scratch/$name-image
  bytes=$(du -sb --apparent-size "$root" | cut -f1)
  mib=$(( (bytes + bytes / 10 + 64 * 1024 * 1024 + 1048575) / 1048576 ))
  rm -rf "$image"
  mkdir -p "$image" "$out"
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
  podman build --timestamp 0 -f "$image/Containerfile" -t "localhost/$name" "$image"
  rm -f "$out/image.oci.tar"
  podman push "localhost/$name" "oci-archive:$out/image.oci.tar"
  archive_digest "$out/image.oci.tar" > "$out/digest"
  printf 'built %s\n' "$(cat "$out/digest")"
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
  session)
    out=${2:?usage: hosted-guest.sh session OUT}
    # The components' cargo arguments and their files both come from
    # os/components.toml: the guest names a component and cannot place a file of
    # its own choosing.
    read -r -a cargo_args <<<"$(python3 os/scripts/install-components.py \
      --dry-run --format cargo "${session_components[@]}")"
    cargo build --locked --release "${cargo_args[@]}"
    rm -rf "$out"
    mkdir -p "$out"
    python3 os/scripts/install-components.py --layer desktop-binaries \
      --from "${CARGO_TARGET_DIR:-$repo_root/target}/release" --dest "$out" "${session_components[@]}"
    # The fonts the session draws with, from the pinned catalog by digest: the
    # system's copy, which fontconfig finds for the session and every app it
    # starts.
    python3 tools/font-catalog.py prepare crates/sdk/ui-core/assets/fonts \
      "$out/usr/share/fonts/tilo" --scope product --offline \
      --cache "${TILO_FONT_ASSET_CACHE:-$scratch/tilo-font-seed}" >/dev/null
    test -x "$out/usr/bin/tilo-session"
    test -x "$out/usr/libexec/tilo/tilo-launcher"
    test -s "$out/usr/share/tilo/system-interfaces.toml"
    test -s "$out"/usr/share/fonts/tilo/manifest.toml
    du -sh "$out"
    ;;
  build)
    session=${2:?usage: hosted-guest.sh build SESSION COMPILED CHROMIUM KERNEL AGENT FONTS OUT}
    compiled=${3:?}
    chromium=${4:?}
    kernel=${5:?}
    agent=${6:?}
    fonts=${7:?}
    out=${8:?}
    [[ $(uname -m) == x86_64 ]] || die 'the hosted guest is built on x86_64'
    container_build_setup
    assets=$(pinned_assets "${guest_assets[@]}")
    . os/hosted-guest/base.env
    guest=localhost/tilo-hosted-guest:ci
    podman build --pull=always -f os/Containerfile.hosted-guest . \
      --build-arg "BASE_REF=$HOSTED_GUEST_BASE_REF" \
      --build-arg "NODE_ASSET=$node_asset" \
      --build-arg "FNM_ASSET=$fnm_asset" \
      --build-arg "RUSTUP_INIT_ASSET=$rustup_init_asset" \
      --build-arg "CHROMIUM_RUNTIME_VERSION=$(chromium_runtime_version)" \
      --build-arg "SOURCE_DATE_EPOCH=$(git show -s --format=%ct "${T_COMMIT:-HEAD}")" \
      --build-context "github-assets=$assets" \
      --build-context "session=$session" \
      --build-context "compiled=$compiled" \
      --build-context "chromium-engine=$chromium" \
      --build-context "font-seed=$fonts" \
      --build-context "hosted-agent=$agent" \
      -t "$guest"
    root=$scratch/hosted-guest-root
    export_rootfs "$guest" "$root"
    # What the guest is, in the build log, and a guest that lost any of it fails
    # here rather than on a host: the init and agent, the toolchains, the
    # session, Browser and its engine.
    test -x "$root/usr/libexec/tilo/hosted-init"
    test -x "$root/usr/libexec/tilo/tilo-legacy-agent"
    test -s "$root/home/tilo/.cargo/bin/rustup"
    test -x "$root/usr/bin/tilo-session"
    test -x "$root/usr/libexec/tilo/browser-session"
    grep -q 'so\.tilo\.browser' "$root/usr/share/tilo-apps/registry.toml"
    test -x "$root/usr/lib/tilo-runtime/$(chromium_runtime_version)/lib/chromium/tilo-chromium-host"
    wrap_guest "$root" "$kernel" "$out" hosted-guest-image
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
  *) die 'usage: hosted-guest.sh kernel|agent|session|build|pins|pin ...' ;;
esac
