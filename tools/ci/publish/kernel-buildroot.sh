#!/usr/bin/env bash
# The kernel buildroot (os/Containerfile.kernel-buildroot): the only build allowed to resolve
# kernel build dependencies and the Fedora kernel SRPM from Fedora. Each architecture is built
# natively; a release tag signs and publishes both, and their pinned references in
# os/kernel-buildroot-snapshot.env, which the kernel build (kernel.sh) and the OS image build FROM,
# are written from what it published.
#
#   kernel-buildroot.sh build ARCH OUT       build and audit OUT/image.oci.tar (buildah image)
#   kernel-buildroot.sh pins OUT...          after publish-kernel-buildroot published each OUT,
#                                            write OUT/../pins.env: the published references and
#                                            the inputs they were built from
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/tilo-kernel-buildroot

# What decides a buildroot's contents besides Fedora's mirrors on the day it is built.
inputs_digest() {
  cat os/Containerfile.kernel-buildroot os/base-snapshot.env | sha256sum | cut -d' ' -f1
}

case ${1:-} in
  build)
    arch=${2:?usage: kernel-buildroot.sh build ARCH OUT}
    out=${3:?}
    case $arch in
      x86_64) platform=linux/amd64 ;;
      aarch64) platform=linux/arm64 ;;
      *) die "unknown architecture $arch" ;;
    esac
    [[ $(uname -m) == "$arch" ]] || die "the $arch buildroot is built natively, not on $(uname -m)"
    . os/base-snapshot.env
    mkdir -p "$scratch/empty"
    bash tools/ci/publish/oci-build.sh "$scratch/empty" os/Containerfile.kernel-buildroot "$out" \
      --platform "$platform" --build-arg "BASE_REF=$BASE_REF"
    # Its architecture and base, its package manifest and no repository to fall back to,
    # exactly one signature-checked kernel SRPM whose recorded checksum still matches, and the
    # build tools: on x86_64 also the aarch64 cross toolchain the aarch64 kernel builds with.
    bash tools/ci/publish/oci-run.sh "$out/image.oci.tar" sh -euc '
      test "$(cat /usr/lib/tilo/kernel-buildroot-arch)" = "$1"
      test "$(cat /usr/lib/tilo/kernel-buildroot-base)" = "$2"
      test -s /usr/lib/tilo/kernel-buildroot-packages.txt
      ! find /etc/yum.repos.d -type f -print -quit | grep -q .
      test "$(find /usr/src/tilo-kernel -maxdepth 1 -name "kernel-*.src.rpm" | wc -l)" -eq 1
      cd /usr/src/tilo-kernel
      sed -n 2p /usr/lib/tilo/kernel-buildroot-srpm.txt | sha256sum --check --strict
      command -v createrepo_c rpmbuild gcc clang cargo pahole ccache >/dev/null
      if [ "$1" = x86_64 ]; then
        test "$(cat /usr/lib/tilo/kernel-buildroot-cross)" = "aarch64 aarch64-linux-gnu-"
        command -v aarch64-linux-gnu-gcc aarch64-linux-gnu-ld aarch64-linux-gnu-strip >/dev/null
      else
        test ! -e /usr/lib/tilo/kernel-buildroot-cross
      fi
      cat /usr/lib/tilo/kernel-buildroot-srpm.txt' sh "$arch" "$BASE_REF"
    ;;
  pins)
    shift
    (($#)) || die 'usage: kernel-buildroot.sh pins OUT...'
    lines=()
    for out in "$@"; do
      arch=$(basename "$out")
      [[ -s $out/receipt.json ]] || die "$out is not published"
      key=KERNEL_BUILDROOT_$(printf '%s' "$arch" | tr '[:lower:]' '[:upper:]')_REF
      lines+=("$key=$repository@$(cat "$out/digest")")
    done
    printf '%s\n' "${lines[@]}" "INPUTS_SHA256=$(inputs_digest)" > "$(dirname -- "$1")/pins.env"
    cat "$(dirname -- "$1")/pins.env"
    ;;
  *) die 'usage: kernel-buildroot.sh build|pins ...' ;;
esac
