#!/usr/bin/env bash
# The build halves of the Windows compatibility payloads (docs/windows-compatibility.md): the
# ARM FEX toolchain, its x86 SDK and the portable FEX payload built from both, and the tooling
# image Wine is rebuilt in (wine.sh). They hold no
# secrets and push nothing; each writes an OCI archive and its digest (oci-build.sh), which the
# publishing checks push with that digest unchanged. Images built from another one's archive
# name it by the registry digest it is published as, so what they record (FEX's inputs.env)
# points at the published image.
#
# usage: windows-compat.sh fex-source ARCHIVE
#        windows-compat.sh wine-tools OUT
#        windows-compat.sh fex-buildroot OUT
#        windows-compat.sh fex-x86-sdk OUT
#        windows-compat.sh fex-payload OUT BUILDROOT_OUT SDK_OUT SOURCE_ARCHIVE
#
# The image builds run in the buildah image as root in a micro-VM step, like oci-build.sh.
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
. "$repo_root/os/base-snapshot.env"
labels=(--label org.tilo.development-only=true)

# The pinned external assets NAME... in a directory of their own, for --build-context.
assets() {
  local dir=$scratch/github-assets
  TILO_GITHUB_ASSET_CACHE=$dir TILO_ASSET_STORE= bash "$repo_root/os/dev/cache-github-assets.sh" "$@" >&2
  printf '%s' "$dir"
}

# Put the image of OUT/image.oci.tar into oci-build.sh's store under REPOSITORY, and print the
# reference it is published as: REPOSITORY@<its digest>. A build then takes it FROM that
# reference without pulling it.
preload() {
  local repository=$1 out=$2 digest id ref
  digest=$(cat "$out/digest")
  [[ $(archive_digest "$out/image.oci.tar") == "$digest" ]] || die "$out/image.oci.tar is not its build"
  id=$(b pull --quiet "oci-archive:$out/image.oci.tar")
  b tag "$id" "$repository:local" >&2
  ref=$repository@$digest
  b inspect --type image "$ref" >/dev/null || die "$ref does not resolve to the archive's image"
  printf '%s' "$ref"
}

cmd=${1:?usage: windows-compat.sh fex-source|wine-tools|fex-buildroot|fex-x86-sdk|fex-payload ...}
shift
case $cmd in
  fex-source)
    archive=${1:?usage: windows-compat.sh fex-source ARCHIVE}
    src=$scratch/fex-source
    rm -rf "$src"
    bash "$repo_root/os/dev/fetch-fex-source.sh" "$src"
    mkdir -p "$(dirname -- "$archive")"
    # The build checks the checkout again, .git and submodules included.
    tar -C "$src" -cf "$archive" .
    ;;
  wine-tools)
    out=${1:?usage: windows-compat.sh wine-tools OUT}
    [[ $(uname -m) == x86_64 ]] || die 'Wine is built natively on x86_64'
    bash "$here/oci-build.sh" "$repo_root" os/Containerfile.wine-tools "$out" \
      --platform linux/amd64 --build-arg "BASE_REF=$BASE_REF"
    ;;
  fex-buildroot)
    out=${1:?usage: windows-compat.sh fex-buildroot OUT}
    [[ $(uname -m) == aarch64 ]] || die 'the FEX toolchain is built on aarch64'
    dir=$(assets nasm-2.16.03.tar.xz)
    bash "$here/oci-build.sh" "$repo_root" os/Containerfile.fex-buildroot "$out" \
      --build-arg "BASE_REF=$BASE_REF" --build-arg "BUILD_JOBS=$(nproc)" \
      --build-context "github-assets=$dir" "${labels[@]}"
    ;;
  fex-x86-sdk)
    out=${1:?usage: windows-compat.sh fex-x86-sdk OUT}
    [[ $(uname -m) == x86_64 ]] || die 'the x86 SDK is built on x86_64, so its scriptlets run natively'
    bash "$here/oci-build.sh" "$repo_root" os/Containerfile.fex-x86-sdk "$out" \
      --platform linux/amd64 --build-arg "BASE_REF=$BASE_REF" "${labels[@]}"
    ;;
  fex-payload)
    out=${1:?usage: windows-compat.sh fex-payload OUT BUILDROOT_OUT SDK_OUT SOURCE_ARCHIVE}
    buildroot_out=${2:?}
    sdk_out=${3:?}
    source_archive=${4:?}
    [[ $(uname -m) == aarch64 ]] || die 'the FEX payload is built on aarch64'
    use_buildah buildah
    buildroot=$(preload registry.tilo.so/tilo/tilo-fex-buildroot-candidate "$buildroot_out")
    sdk=$(preload registry.tilo.so/tilo/tilo-fex-x86-sdk-candidate "$sdk_out")
    src=$scratch/fex-source
    rm -rf "$src"
    mkdir -p "$src"
    # Owned by root, who builds it, so Git trusts the checkout build-fex-payload.sh verifies.
    tar -C "$src" --no-same-owner -xf "$source_archive"
    # Bounded by the step's cores and memory: about 4 GiB per compiler.
    jobs=$(( $(awk '/^MemTotal:/ {print int($2 / 4194304)}' /proc/meminfo) ))
    (( jobs > $(nproc) )) && jobs=$(nproc)
    (( jobs < 1 )) && jobs=1
    printf 'FEX payload build jobs=%s (cores %s)\n' "$jobs" "$(nproc)"
    bash "$here/oci-build.sh" "$repo_root" os/Containerfile.fex-payload "$out" --pull=never \
      --build-arg "FEX_BUILDROOT_REF=$buildroot" --build-arg "FEX_X86_SDK_REF=$sdk" \
      --build-arg "BUILD_JOBS=$jobs" --build-context "fex-source=$src" "${labels[@]}"
    ;;
  *) die "unknown command $cmd" ;;
esac
