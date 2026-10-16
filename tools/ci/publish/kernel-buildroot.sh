#!/usr/bin/env bash
# The kernel buildroot (os/Containerfile.kernel-buildroot): the only build allowed to resolve
# kernel build dependencies and the Fedora kernel SRPM from Fedora. Each architecture is built
# natively; stable signs and publishes both, and main pins them in
# os/kernel-buildroot-snapshot.env, which build-kernel.yml and the OS image build FROM.
#
#   kernel-buildroot.sh build ARCH OUT       build and audit OUT/image.oci.tar (buildah image)
#   kernel-buildroot.sh pins OUT...          after publish-kernel-buildroot published each OUT,
#                                            write OUT/../pins.env: the published references and
#                                            the inputs they were built from
#   kernel-buildroot.sh pin                  write os/kernel-buildroot-snapshot.env from stable's
#                                            pins (T_POINTER_KERNEL_BUILDROOT), when main builds
#                                            the buildroot from the same inputs
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/tilo-kernel-buildroot
pins=os/kernel-buildroot-snapshot.env

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
    # build tools.
    bash tools/ci/publish/oci-run.sh "$out/image.oci.tar" sh -euc '
      test "$(cat /usr/lib/tilo/kernel-buildroot-arch)" = "$1"
      test "$(cat /usr/lib/tilo/kernel-buildroot-base)" = "$2"
      test -s /usr/lib/tilo/kernel-buildroot-packages.txt
      ! find /etc/yum.repos.d -type f -print -quit | grep -q .
      test "$(find /usr/src/tilo-kernel -maxdepth 1 -name "kernel-*.src.rpm" | wc -l)" -eq 1
      cd /usr/src/tilo-kernel
      sed -n 2p /usr/lib/tilo/kernel-buildroot-srpm.txt | sha256sum --check --strict
      command -v createrepo_c rpmbuild gcc clang cargo pahole >/dev/null
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
  pin)
    [[ -n ${T_POINTER_KERNEL_BUILDROOT:-} ]] || { echo 'stable has published no kernel buildroot yet'; exit 0; }
    x86=$(sed -n 's/^KERNEL_BUILDROOT_X86_64_REF=//p' <<<"$T_POINTER_KERNEL_BUILDROOT")
    arm=$(sed -n 's/^KERNEL_BUILDROOT_AARCH64_REF=//p' <<<"$T_POINTER_KERNEL_BUILDROOT")
    built_from=$(sed -n 's/^INPUTS_SHA256=//p' <<<"$T_POINTER_KERNEL_BUILDROOT")
    # Stable's buildroot is pinned only where main would build the same one: a change to the
    # buildroot's inputs on main waits for stable to publish it.
    if [[ $built_from != "$(inputs_digest)" ]]; then
      echo "stable's kernel buildroot was built from other inputs than main's; leaving the pins"
      exit 0
    fi
    for ref in "$x86" "$arm"; do
      [[ $ref =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]] \
        || die "not an immutable kernel buildroot reference: $ref"
    done
    [[ $x86 != "$arm" ]] || die 'native buildroots unexpectedly share one manifest'
    use_cosign
    for ref in "$x86" "$arm"; do
      cosign verify --key keys/cosign.pub "$ref" >/dev/null
    done
    python3 os/dev/open-snapshot-pr.py pins kernel-buildroot --x86-ref "$x86" --arm-ref "$arm" > "$pins"
    printf 'build(os): refresh native kernel buildroot snapshots\n' > .t-commit-message
    cat "$pins"
    ;;
  *) die 'usage: kernel-buildroot.sh build|pins|pin ...' ;;
esac
