#!/usr/bin/env bash
# The Tilo kernel (os/kernel/Containerfile): Fedora's kernel rebuilt with Tilo's config fragment
# FROM the signed native kernel buildroot that os/kernel-buildroot-snapshot.env pins, packaged as
# an RPM repository image, booted under emulation and checked against the firmware's Secure Boot.
#
#   kernel.sh build ARCH OUT    verify the pinned buildroot, build OUT/image.oci.tar and the
#                               Secure Boot probe's inputs into OUT/secureboot (buildah image,
#                               as root in a micro-VM step)
#
# kernel-acceptance.sh, beside this script, boots and checks what this builds.
#
#   kernel.sh provenance DIR    write each DIR/<arch>/provenance.json: the kernel-rpm/v1 SLSA
#                               predicate os/dev/verify-kernel-provenance.sh checks, binding the
#                               signed buildroot it was built FROM (no secrets; the commit is
#                               left as {commit} for oci-attest.sh)
#   kernel.sh pins DIR          after publish-kernel published both, write DIR/pins.env: the
#                               published references and the inputs they were built from
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
      key=KERNEL_BUILDROOT_$(printf '%s' "$arch" | tr '[:lower:]' '[:upper:]')_REF
      digest=$(cat "$dir/$arch/digest")
      [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "not a digest: $digest"
      buildroot=${!key}
      [[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]] \
        || die "no digest-pinned native kernel buildroot for $arch"
      # kernel.sh build leaves SOURCE_DATE_EPOCH at the Containerfile's default of 0.
      printf '%s\n' '{' \
        '  "buildDefinition": {' \
        '    "buildType": "https://tilo.sh/buildtypes/kernel-rpm/v1",' \
        "    \"externalParameters\": {\"architecture\": \"$arch\"}," \
        "    \"internalParameters\": {\"sourceDateEpoch\": 0, \"expectedReleaseDigest\": \"$digest\"}," \
        "    \"resolvedDependencies\": [{\"uri\": \"git+ssh://code.tilo.so/tilo/tilo@{commit}\"}, {\"uri\": \"$buildroot\"}]" \
        '  },' \
        '  "runDetails": {"builder": {"id": "https://code.tilo.so/t/checks/publish-kernel"}, "metadata": {}}' \
        '}' > "$dir/$arch/provenance.json"
      printf '%s %s FROM %s\n' "$arch" "$digest" "$buildroot"
    done
    exit 0
    ;;
  pins)
    dir=${2:?usage: kernel.sh pins DIR}
    lines=()
    for arch in x86_64 aarch64; do
      [[ -s $dir/$arch/receipt.json ]] || die "$dir/$arch is not published"
      key=KERNEL_RPMS_$(printf '%s' "$arch" | tr '[:lower:]' '[:upper:]')_REF
      lines+=("$key=$repository@$(cat "$dir/$arch/digest")")
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
  x86_64) platform=linux/amd64 key=KERNEL_BUILDROOT_X86_64_REF ;;
  aarch64) platform=linux/arm64 key=KERNEL_BUILDROOT_AARCH64_REF ;;
  *) die 'usage: kernel.sh build ARCH OUT, or provenance|pins DIR, or pin' ;;
esac
[[ -n $out ]] || die 'usage: kernel.sh build ARCH OUT'
[[ $(uname -m) == "$arch" ]] || die "the $arch kernel is built natively, not on $(uname -m)"

case ${1:-} in
  build)
    . os/kernel-buildroot-snapshot.env
    buildroot=${!key}
    [[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]] \
      || die "no digest-pinned native kernel buildroot for $arch"
    use_cosign
    cosign verify --key keys/cosign.pub "$buildroot" >/dev/null
    assets=$scratch/kernel-assets
    TILO_GITHUB_ASSET_CACHE=$assets TILO_ASSET_STORE= bash os/dev/cache-github-assets.sh \
      v4l2loopback-0.15.3-a7e4a4d.tar.gz "sccache-v0.17.0-$arch-unknown-linux-musl.tar.gz"
    # No compiler cache reaches checks yet; the kernel's SOURCE_DATE_EPOCH keeps its default of
    # 0 beside the fixed layer timestamps, so the build is the same from the same inputs.
    args=(--platform "$platform" --build-context "github-assets=$assets"
      --build-arg "KERNEL_BUILDROOT_REF=$buildroot" --build-arg "KERNEL_BUILD_JOBS=$(nproc)"
      --build-arg TILO_COMPILER_CACHE=off --build-arg TILO_REQUIRE_COMPILER_CACHE=0)
    # Both targets share the build stage: one layered store compiles the kernel once.
    use_buildah kernel
    # The micro-VM step's scratch disk is ext4 under a kernel of its own, where overlay works;
    # vfs would copy the multi-gigabyte buildroot for every step and fill the disk.
    export STORAGE_DRIVER=overlay
    printf '[storage]\ndriver = "overlay"\n' > "$CONTAINERS_STORAGE_CONF"
    b build --layers --timestamp 0 --format oci --file os/kernel/Containerfile "${args[@]}" \
      -t localhost/tilo-kernel:build .
    b build --layers --timestamp 0 --file os/kernel/Containerfile --target secureboot-probe-inputs \
      "${args[@]}" --output "type=local,dest=$out/secureboot" .
    rm -f "$out/image.oci.tar"
    b push --compression-format gzip --force-compression localhost/tilo-kernel:build \
      "oci-archive:$out/image.oci.tar"
    archive_digest "$out/image.oci.tar" > "$out/digest"
    printf 'built %s\n' "$(cat "$out/digest")"
    ;;
  *) die 'usage: kernel.sh build ARCH OUT' ;;
esac
