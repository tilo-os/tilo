# shellcheck shell=bash
# What both kernel builds share, sourced after lib.sh at the repository's root: the kernel build
# (kernel.sh build) and the compiler cache it compiles through (kernel-compile-cache.sh). Kept
# apart from kernel.sh, so its publishing, pinning and provenance steps key no compiler cache.

# kernel_signer_crate OUT: the module signer as the kernel compiles it, tilo-crypto projected
# into a crate of its own whose lockfile holds only what it links, checked against the root
# lockfile. The kernel builds from this, never from the workspace, so a Cargo.lock change that
# moves none of the signer's dependencies leaves the kernel's key where it was.
kernel_signer_crate() {
  local out=$1
  rm -rf "$out" && mkdir -p "$out/src"
  python3 os/kernel/project-crate.py --workspace Cargo.toml \
    --member crates/system/tilo-crypto/Cargo.toml --lockfile Cargo.lock --output "$out"
  cp crates/system/tilo-crypto/src/lib.rs crates/system/tilo-crypto/src/main.rs "$out/src/"
  # Resolving the projected crate prunes the copied lockfile to its own dependencies. Where
  # there is no cargo (the buildah image) it stays the whole lockfile, which the kernel build's
  # own cargo prunes as it resolves.
  if command -v cargo >/dev/null; then
    (cd "$out" && { cargo fetch --offline 2>/dev/null || cargo fetch; })
    python3 os/kernel/project-crate.py --lockfile Cargo.lock --verify-lock "$out/Cargo.lock"
  fi
  printf 'signer crate: %s locked packages\n' "$(grep -c '^\[\[package\]\]' "$out/Cargo.lock")"
}

# kernel_build_setup ARCH: verify the buildroot pinned for this machine and set what a build of
# os/kernel/Containerfile for ARCH takes: `args` (the build's arguments), `buildroot`, `host`,
# `platform`, `kernel_platform`, `assets` (the pinned external assets) and buildah's storage.
# Natively, or the aarch64 kernel cross-compiled on x86_64 with the cross toolchain the x86_64
# buildroot carries: the build runs FROM the buildroot of the machine it runs on.
kernel_build_setup() {
  local arch=$1 key signer
  case $arch in
    x86_64) kernel_platform=linux/amd64 ;;
    aarch64) kernel_platform=linux/arm64 ;;
    *) die "no kernel for $arch" ;;
  esac
  host=$(uname -m)
  case $host:$arch in
    x86_64:x86_64 | aarch64:aarch64 | x86_64:aarch64) ;;
    *) die "the $arch kernel is built natively or on x86_64, not on $host" ;;
  esac
  case $host in
    x86_64) platform=linux/amd64 key=KERNEL_BUILDROOT_X86_64_REF ;;
    aarch64) platform=linux/arm64 key=KERNEL_BUILDROOT_AARCH64_REF ;;
  esac
  . os/kernel-buildroot-snapshot.env
  buildroot=${!key}
  [[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]] \
    || die "no digest-pinned kernel buildroot for $host"
  use_cosign
  cosign verify --key keys/cosign.pub "$buildroot" >/dev/null
  assets=$(pinned_assets v4l2loopback-0.15.3-a7e4a4d.tar.gz)
  # The signer's crate the kernel-signer-crate check made, or one made here where no check gave
  # it (the compiler cache, a build outside t).
  signer=${KERNEL_SIGNER_CRATE:-/target/publish/kernel-signer}
  if [[ ! -s $signer/Cargo.lock ]]; then
    signer=$scratch/signer-crate
    kernel_signer_crate "$signer"
  fi
  # The kernel's SOURCE_DATE_EPOCH keeps its default of 0 beside the fixed layer timestamps.
  args=(--platform "$platform" --build-context "github-assets=$assets"
    --build-context "signer-crate=$signer"
    --build-arg "KERNEL_BUILDROOT_REF=$buildroot" --build-arg "KERNEL_ARCH=$arch"
    --build-arg "KERNEL_BUILD_JOBS=$(nproc)")
  use_buildah kernel
  # The micro-VM step's scratch disk is ext4 under a kernel of its own, where overlay works;
  # vfs would copy the multi-gigabyte buildroot for every step and fill the disk.
  export STORAGE_DRIVER=overlay
  printf '[storage]\ndriver = "overlay"\n' > "$CONTAINERS_STORAGE_CONF"
}
