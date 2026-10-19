#!/usr/bin/env bash
# Incremental build of the Tilo kernel for dev VMs (`tilo-vm swap kernel`).
#
#   os/kernel/dev-build.sh [--jobs N] [--direct-initrd] [--result FILE]
#
# The same kernel os/kernel/Containerfile builds, without its 60-90 minutes on
# every change:
#
#   * the builder is that Containerfile's own `build-toolchain` stage (the
#     pinned kernel buildroot's toolchain, tilo-crypto, the Fedora SRPM after
#     %prep), built with podman and reused while its inputs are unchanged;
#   * the prepared source tree is copied out of it once per SRPM version into
#     ${TILO_KERNEL_CACHE:-~/.cache/tilo-kernel}/<linux-version>/ and kept, with
#     its Kbuild objects, so the next `make` recompiles only what changed;
#   * every step on that tree is os/kernel/build-support.sh (prepare-source,
#     compile, stage-modules), with the release build's /work layout, fragments,
#     patches, checks and build environment. Two deliberate differences: the
#     module signing identity lives as long as the cache instead of one
#     build, and the kernel release carries a `+dev` suffix (LOCALVERSION).
#     The image's kernel is built from the same SRPM, so without it a dev
#     kernel's /usr/lib/modules/<release> would shadow the image kernel's,
#     whose signing key rejects dev-signed modules, and the image kernel
#     could no longer serve as the fallback boot entry.
#
# Jobs default to half the CPUs (other builds share the host's memory);
# override with --jobs or TILO_KERNEL_JOBS. The result (kernel release, staged
# tree, vmlinuz, timings) is written as JSON to --result.
#
# --direct-initrd also writes a generic initramfs (the buildroot base's dracut,
# with virtio_net) for booting the kernel straight from the host (libvirt
# direct kernel boot). It is not the image's initramfs; installing into the VM
# (`tilo-vm swap kernel`) builds that one in the VM.
set -euo pipefail

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cache_root="${TILO_KERNEL_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/tilo-kernel}"
jobs="${TILO_KERNEL_JOBS:-}"
direct_initrd=0
result=""

die() { printf 'dev-build: %s\n' "$*" >&2; exit 1; }
say() { printf '[tilo-kernel] %s\n' "$*" >&2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --jobs) jobs="${2:?--jobs needs a number}"; shift 2 ;;
    --direct-initrd) direct_initrd=1; shift ;;
    --result) result="${2:?--result needs a path}"; shift 2 ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

if [ -z "$jobs" ]; then
  jobs=$(( $(nproc) / 2 ))
  [ "$jobs" -ge 1 ] || jobs=1
fi
case "$jobs" in ''|*[!0-9]*|0) die "--jobs must be a positive integer, got: $jobs" ;; esac
command -v podman >/dev/null || die "podman is required (rootless) to run the kernel builder image"

arch="$(uname -m)"
case "$arch" in x86_64|aarch64) ;; *) die "unsupported kernel architecture: $arch" ;; esac

# The native buildroot pin the release build uses (the workflow passes the same).
case "$arch" in
  x86_64) buildroot_var=KERNEL_BUILDROOT_X86_64_REF ;;
  aarch64) buildroot_var=KERNEL_BUILDROOT_AARCH64_REF ;;
esac
KERNEL_BUILDROOT_REF="${KERNEL_BUILDROOT_REF:-$(sed -n "s/^$buildroot_var=//p" "$repo/os/kernel-buildroot-snapshot.env")}"
[ -n "$KERNEL_BUILDROOT_REF" ] || die "os/kernel-buildroot-snapshot.env has no $buildroot_var; run the build-buildroots workflow"

mkdir -p "$cache_root"
started=$(date +%s)

# ---- builder image ----------------------------------------------------------
# Keyed by everything the build-toolchain stage reads, so an unchanged builder
# is not even re-sent to podman.
key="$(
  {
    printf '%s\n%s\n' "$KERNEL_BUILDROOT_REF" "$arch"
    cd "$repo"
    find os/kernel/Containerfile os/kernel/project-crate.py os/tests/secureboot_vm_app.c \
         Cargo.toml Cargo.lock crates/system/tilo-crypto/Cargo.toml \
         crates/system/tilo-crypto/src/lib.rs crates/system/tilo-crypto/src/main.rs \
         os/external-assets/github-assets.tsv -type f -print0 \
      | sort -z | xargs -0 sha256sum
  } | sha256sum | cut -c1-16
)"
image="localhost/tilo-kernel-builder:$arch-$key"
if podman image exists "$image"; then
  say "builder image $image (cached)"
else
  say "building the builder image $image (os/kernel/Containerfile --target build-toolchain)"
  assets="$cache_root/assets"
  TILO_GITHUB_ASSET_CACHE="$assets" "$repo/os/dev/cache-github-assets.sh" v4l2loopback-0.15.3-a7e4a4d.tar.gz >&2
  podman build -f "$repo/os/kernel/Containerfile" --target build-toolchain \
    --build-arg "KERNEL_BUILDROOT_REF=$KERNEL_BUILDROOT_REF" \
    --build-context "github-assets=$assets" \
    -t "$image" "$repo" >&2
fi
builder_id="$(podman image inspect --format '{{.Id}}' "$image")"

# ---- persistent tree ----------------------------------------------------------
tree_name="$(podman run --rm "$image" sh -c \
  "find /work/rpmbuild/BUILD -maxdepth 4 -type d -name 'linux-*' | head -1 | xargs -r basename")"
[ -n "$tree_name" ] || die "the builder image has no prepared kernel source tree under /work/rpmbuild/BUILD"
cache="$cache_root/$tree_name"
mkdir -p "$cache/out"
exec 9>"$cache/.lock"
flock --nonblock 9 || die "another kernel build is using $cache; wait for it or remove $cache/.lock if none runs"

# The release build's environment (its ENV block, SOURCE_DATE_EPOCH at its
# default), plus the dev release suffix.
run() {
  podman run --rm --network=none --security-opt label=disable \
    -e KBUILD_BUILD_TIMESTAMP=@0 \
    -e KBUILD_BUILD_USER=tilo \
    -e KBUILD_BUILD_HOST=tilo-build \
    -e LC_ALL=C.UTF-8 \
    -e TZ=UTC \
    -e LOCALVERSION=+dev \
    -v "$cache/rpmbuild:/work/rpmbuild" \
    -v "$cache/module-signing:/work/module-signing" \
    -v "$cache/out:/work/out" \
    -v "$repo/os/kernel/config-fragments:/work/kernel-config:ro" \
    -v "$repo/os/kernel/patches:/work/kernel-patches:ro" \
    -v "$repo/os/kernel/patches.toml:/work/kernel-patches.toml:ro" \
    -v "$repo/os/kernel/check-patches.py:/work/check-patches.py:ro" \
    -v "$repo/os/kernel/build-support.sh:/work/kernel-build-support.sh:ro" \
    -v "$repo/os/kernel/cc-wrapper.sh:/work/kernel-cc-wrapper.sh:ro" \
    -v "$repo/os/kernel/check-signing-identity.py:/work/check-signing-identity.py:ro" \
    -v "$repo/os/kernel/check-config.py:/work/check-config.py:ro" \
    "$image" "$@"
}

if [ ! -e "$cache/rpmbuild/.tilo-complete" ]; then
  say "first build of $tree_name: copying the prepared source tree into $cache"
  rm -rf "$cache/rpmbuild"
  mkdir -p "$cache/rpmbuild"
  podman run --rm --network=none --security-opt label=disable \
    -v "$cache:/cache" "$image" cp -a /work/rpmbuild/. /cache/rpmbuild/
  touch "$cache/rpmbuild/.tilo-complete"
fi
if [ ! -s "$cache/module-signing/signing-cert.der" ]; then
  mkdir -p "$cache/module-signing"
  run sh /work/kernel-build-support.sh signing-key "$arch" /work/module-signing
fi

# ---- build --------------------------------------------------------------------
say "building $tree_name with $jobs jobs (log: $cache/build.log)"
compile_started=$(date +%s)
run sh -euc '
  arch=$1 jobs=$2
  KSRC="$(find /work/rpmbuild/BUILD -maxdepth 4 -type d -name "linux-*" | head -1)"
  sh /work/kernel-build-support.sh validate-arch "$arch"
  sh /work/kernel-build-support.sh prepare-source "$arch" "$KSRC"
  sh /work/kernel-build-support.sh compile "$arch" "$KSRC" "$jobs" /work/v4l2loopback-0.15.3
  KVER="$(make -s -C "$KSRC" kernelrelease)"
  sh /work/kernel-build-support.sh stage-modules "$arch" "$KSRC" /work/out/stage "$KVER" \
    /work/v4l2loopback-0.15.3 /work/module-signing "$jobs"
  printf "%s\n" "$KVER" > /work/out/kver
  sed -n "s/^#define UTS_VERSION \"\(.*\)\"$/\1/p" "$KSRC/include/generated/utsversion.h" > /work/out/uts-version
' _ "$arch" "$jobs" 2>&1 | tee "$cache/build.log" >&2
compile_seconds=$(( $(date +%s) - compile_started ))
kver="$(cat "$cache/out/kver")"
uts_version="$(cat "$cache/out/uts-version")"
modules="$cache/out/stage/lib/modules/$kver"
[ -s "$modules/vmlinuz" ] || die "the build left no $modules/vmlinuz"

initrd=""
if [ "$direct_initrd" = 1 ]; then
  say "writing the direct-boot initramfs"
  podman run --rm --network=none --security-opt label=disable \
    -v "$cache/out:/work/out" \
    -v "$modules:/usr/lib/modules/$kver:ro" "$image" \
    dracut --quiet --force --reproducible --no-hostonly --compress zstd \
      --kver "$kver" --add-drivers "virtio_net" \
      /work/out/initrd-direct.img >&2
  initrd="$cache/out/initrd-direct.img"
fi

seconds=$(( $(date +%s) - started ))
say "kernel $kver ($uts_version) built in ${seconds}s (compile and stage ${compile_seconds}s)"
if [ -n "$result" ]; then
  python3 - "$result" <<EOF
import json, sys
json.dump({
    "kver": "$kver",
    "uts_version": """$uts_version""",
    "modules": "$modules",
    "vmlinuz": "$modules/vmlinuz",
    "initrd": "$initrd" or None,
    "cache": "$cache",
    "builder_image": "$image",
    "builder_image_id": "$builder_id",
    "jobs": $jobs,
    "build_seconds": $seconds,
    "compile_seconds": $compile_seconds,
    "log": "$cache/build.log",
}, open(sys.argv[1], "w"), indent=2)
EOF
fi
