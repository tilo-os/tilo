#!/usr/bin/env bash
# The Tilo kernel (os/kernel/Containerfile): Fedora's kernel rebuilt with Tilo's config fragment
# FROM a signed kernel buildroot that os/kernel-buildroot-snapshot.env pins, packaged as an RPM
# repository image, booted under emulation and checked against the firmware's Secure Boot. The
# build runs FROM the buildroot of the machine it runs on: the x86_64 kernel on x86_64, the
# aarch64 kernel natively or cross-compiled on x86_64 with the aarch64 toolchain the x86_64
# buildroot carries.
#
#   kernel.sh compile-cache ARCH OUT
#                               compile the kernel and its modules into an empty compiler cache
#                               and pack it as OUT/compiler-cache.tar (buildah, as root in a
#                               micro-VM step)
#   kernel.sh build ARCH OUT [CACHE]
#                               verify the pinned buildroot, build OUT/image.oci.tar and the
#                               Secure Boot probe's inputs into OUT/secureboot, through the
#                               compiler cache CACHE (compile-cache's archive) if given; record
#                               the buildroot and the machine's architecture for provenance
#
# kernel-acceptance.sh, beside this script, boots and checks what this builds, on the kernel's
# architecture.
#
#   kernel.sh provenance DIR    write each DIR/<arch>/provenance.json: the kernel-rpm/v1 SLSA
#                               predicate os/dev/verify-kernel-provenance.sh checks, binding the
#                               signed buildroot it was built FROM and the architecture it was
#                               built on (no secrets; the commit is left as {commit} for
#                               oci-attest.sh)
#   kernel.sh pins DIR          after publish-kernel published both, write DIR/pins.env: the
#                               published references (each receipt's digest, which is an earlier
#                               run's where the tag was published already) and the inputs they
#                               were built from
#   kernel.sh pin               write os/kernel-snapshot.env from stable's pins
#                               (T_POINTER_KERNEL), when main builds the kernel from the same inputs
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/tilo-kernel

# What decides a kernel besides the buildroot's own contents: the Containerfile and config, the
# buildroot pins and the module signer compiled into it. Cargo.lock is left out, since nearly
# every commit moves it; the signer's crate is projected from it with its own dependencies.
inputs_digest() {
  find os/kernel os/kernel-buildroot-snapshot.env crates/system/tilo-crypto/Cargo.toml \
    crates/system/tilo-crypto/src/lib.rs crates/system/tilo-crypto/src/main.rs \
    os/tests/secureboot_vm_app.c -type f -print0 \
    | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1
}

case ${1:-} in
  provenance)
    dir=${2:?usage: kernel.sh provenance DIR}
    . os/kernel-buildroot-snapshot.env
    for arch in x86_64 aarch64; do
      digest=$(cat "$dir/$arch/digest")
      [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "not a digest: $digest"
      # The buildroot and machine kernel.sh build recorded: the one pinned for the architecture
      # it ran on, which is the kernel's own or, for a cross-compiled aarch64 kernel, x86_64.
      built_on=$(cat "$dir/$arch/build-architecture")
      case $built_on:$arch in
        x86_64:x86_64 | aarch64:aarch64 | x86_64:aarch64) ;;
        *) die "the $arch kernel says it was built on $built_on" ;;
      esac
      key=KERNEL_BUILDROOT_$(printf '%s' "$built_on" | tr '[:lower:]' '[:upper:]')_REF
      buildroot=$(cat "$dir/$arch/buildroot")
      [[ $buildroot == "${!key}" ]] \
        || die "the $arch kernel was built FROM $buildroot, not the $built_on buildroot pinned here"
      [[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]] \
        || die "no digest-pinned kernel buildroot for $built_on"
      # kernel.sh build leaves SOURCE_DATE_EPOCH at the Containerfile's default of 0.
      printf '%s\n' '{' \
        '  "buildDefinition": {' \
        '    "buildType": "https://tilo.sh/buildtypes/kernel-rpm/v1",' \
        "    \"externalParameters\": {\"architecture\": \"$arch\"}," \
        "    \"internalParameters\": {\"sourceDateEpoch\": 0, \"expectedReleaseDigest\": \"$digest\", \"buildArchitecture\": \"$built_on\"}," \
        "    \"resolvedDependencies\": [{\"uri\": \"git+ssh://code.tilo.so/tilo/tilo@{commit}\"}, {\"uri\": \"$buildroot\"}]" \
        '  },' \
        '  "runDetails": {"builder": {"id": "https://code.tilo.so/t/checks/publish-kernel"}, "metadata": {}}' \
        '}' > "$dir/$arch/provenance.json"
      printf '%s %s built on %s FROM %s\n' "$arch" "$digest" "$built_on" "$buildroot"
    done
    exit 0
    ;;
  pins)
    dir=${2:?usage: kernel.sh pins DIR}
    lines=()
    for arch in x86_64 aarch64; do
      [[ -s $dir/$arch/receipt.json ]] || die "$dir/$arch is not published"
      key=KERNEL_RPMS_$(printf '%s' "$arch" | tr '[:lower:]' '[:upper:]')_REF
      digest=$(sed -n 's/.*"digest":"\(sha256:[0-9a-f]\{64\}\)".*/\1/p' "$dir/$arch/receipt.json")
      [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "$dir/$arch/receipt.json names no digest"
      lines+=("$key=$repository@$digest")
    done
    printf '%s\n' "${lines[@]}" "INPUTS_SHA256=$(inputs_digest)" > "$dir/pins.env"
    cat "$dir/pins.env"
    exit 0
    ;;
  pin)
    [[ -n ${T_POINTER_KERNEL:-} ]] || { echo 'stable has published no kernel yet'; exit 0; }
    x86=$(sed -n 's/^KERNEL_RPMS_X86_64_REF=//p' <<<"$T_POINTER_KERNEL")
    arm=$(sed -n 's/^KERNEL_RPMS_AARCH64_REF=//p' <<<"$T_POINTER_KERNEL")
    built_from=$(sed -n 's/^INPUTS_SHA256=//p' <<<"$T_POINTER_KERNEL")
    # Stable's kernels are pinned only where main would build the same ones: a change to the
    # kernel's inputs on main waits for stable to publish it.
    if [[ $built_from != "$(inputs_digest)" ]]; then
      echo "stable's kernels were built from other inputs than main's; leaving the pins"
      exit 0
    fi
    for ref in "$x86" "$arm"; do
      [[ $ref =~ ^registry\.tilo\.so/tilo/tilo-kernel@sha256:[0-9a-f]{64}$ ]] \
        || die "not an immutable kernel reference: $ref"
    done
    [[ $x86 != "$arm" ]] || die 'native kernels unexpectedly share one manifest'
    use_cosign
    # As the OS build verifies them: the signature, and provenance naming a signed buildroot.
    for ref in "$x86" "$arm"; do
      arch=x86_64
      [[ $ref == "$arm" ]] && arch=aarch64
      bash os/dev/verify-kernel-provenance.sh "$ref" --architecture "$arch" >/dev/null
    done
    python3 os/dev/open-snapshot-pr.py pins kernel --x86-ref "$x86" --arm-ref "$arm" > os/kernel-snapshot.env
    printf 'build(os): refresh native kernel snapshots\n' > .t-commit-message
    cat os/kernel-snapshot.env
    exit 0
    ;;
esac

arch=${2:-}
out=${3:-}
case $arch in
  x86_64) kernel_platform=linux/amd64 ;;
  aarch64) kernel_platform=linux/arm64 ;;
  *) die 'usage: kernel.sh build ARCH OUT [CACHE], compile-cache ARCH OUT, provenance|pins DIR, or pin' ;;
esac
[[ -n $out ]] || die 'usage: kernel.sh build ARCH OUT [CACHE], or compile-cache ARCH OUT'
# Natively, or the aarch64 kernel cross-compiled on x86_64 with the cross toolchain the x86_64
# buildroot carries: the build runs FROM the buildroot of the machine it runs on.
host=$(uname -m)
case $host:$arch in
  x86_64:x86_64 | aarch64:aarch64 | x86_64:aarch64) ;;
  *) die "the $arch kernel is built natively or on x86_64, not on $host" ;;
esac
case $host in
  x86_64) platform=linux/amd64 key=KERNEL_BUILDROOT_X86_64_REF ;;
  aarch64) platform=linux/arm64 key=KERNEL_BUILDROOT_AARCH64_REF ;;
esac

case ${1:-} in
  build | compile-cache)
    . os/kernel-buildroot-snapshot.env
    buildroot=${!key}
    [[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]] \
      || die "no digest-pinned kernel buildroot for $host"
    use_cosign
    cosign verify --key keys/cosign.pub "$buildroot" >/dev/null
    assets=$(pinned_assets v4l2loopback-0.15.3-a7e4a4d.tar.gz "sccache-v0.17.0-$host-unknown-linux-musl.tar.gz")
    # The kernel's SOURCE_DATE_EPOCH keeps its default of 0 beside the fixed layer timestamps.
    args=(--platform "$platform" --build-context "github-assets=$assets"
      --build-arg "KERNEL_BUILDROOT_REF=$buildroot" --build-arg "KERNEL_ARCH=$arch"
      --build-arg "KERNEL_BUILD_JOBS=$(nproc)")
    use_buildah kernel
    # The micro-VM step's scratch disk is ext4 under a kernel of its own, where overlay works;
    # vfs would copy the multi-gigabyte buildroot for every step and fill the disk.
    export STORAGE_DRIVER=overlay
    printf '[storage]\ndriver = "overlay"\n' > "$CONTAINERS_STORAGE_CONF"
    mkdir -p "$out"
    ;;
  *) die 'usage: kernel.sh build ARCH OUT [CACHE], or compile-cache ARCH OUT' ;;
esac
started=$SECONDS

case $1 in
  compile-cache)
    # Every object of the kernel and its modules, compiled into an empty directory and packed
    # as OUT/compiler-cache.tar (sccache stores each entry compressed already).
    cache=$scratch/compiler-cache
    rm -rf "$cache" && mkdir -p "$cache"
    b build --layers --timestamp 0 --file os/kernel/Containerfile --target compile-cache \
      "${args[@]}" --volume "$cache:/work/compiler-cache" .
    tar -cf "$out/compiler-cache.tar" -C "$cache" .
    printf 'compiler cache of the %s kernel: %s in %ss\n' "$arch" \
      "$(du -sh "$out/compiler-cache.tar" | cut -f1)" "$((SECONDS - started))"
    ;;
  build)
    # With CACHE (compile-cache's compiler-cache.tar), the compile reads and writes a copy of
    # it; without, it compiles every object.
    cache_archive=${4:-}
    if [[ -n $cache_archive ]]; then
      [[ -s $cache_archive ]] || die "no compiler cache at $cache_archive"
      cache=$scratch/compiler-cache
      rm -rf "$cache" && mkdir -p "$cache"
      tar -xf "$cache_archive" -C "$cache"
      args+=(--build-arg TILO_COMPILER_CACHE=local --volume "$cache:/work/compiler-cache")
    else
      args+=(--build-arg TILO_COMPILER_CACHE=off)
    fi
    # Both targets share the build stage: one layered store compiles the kernel once.
    b build --layers --timestamp 0 --file os/kernel/Containerfile --target secureboot-probe-inputs \
      "${args[@]}" --output "type=local,dest=$out/secureboot" .
    if [[ $host == "$arch" ]]; then
      b build --layers --timestamp 0 --format oci --file os/kernel/Containerfile "${args[@]}" \
        -t localhost/tilo-kernel:build .
    else
      # The repository the build stage made on this machine, in an image for the kernel's
      # platform: what the OS build pulls and verify-kernel-provenance.sh checks.
      rm -rf "$scratch/kernel-repo"
      b build --layers --timestamp 0 --file os/kernel/Containerfile "${args[@]}" \
        --output "type=local,dest=$scratch/kernel-repo" .
      b build --layers --timestamp 0 --format oci --file os/kernel/Containerfile --target publish-cross \
        --platform "$kernel_platform" --build-arg "KERNEL_BUILDROOT_REF=$buildroot" \
        --build-context "github-assets=$assets" --build-context "kernel-repo=$scratch/kernel-repo/repo" \
        -t localhost/tilo-kernel:build .
    fi
    rm -f "$out/image.oci.tar"
    b push --compression-format gzip --force-compression localhost/tilo-kernel:build \
      "oci-archive:$out/image.oci.tar"
    archive_digest "$out/image.oci.tar" > "$out/digest"
    # What provenance names: the buildroot this ran FROM, and the machine's architecture.
    printf '%s\n' "$buildroot" > "$out/buildroot"
    printf '%s\n' "$host" > "$out/build-architecture"
    printf 'built %s (%s kernel on %s) in %ss\n' "$(cat "$out/digest")" "$arch" "$host" "$((SECONDS - started))"
    ;;
esac
