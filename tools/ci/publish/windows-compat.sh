#!/usr/bin/env bash
# The build halves of the Windows compatibility payloads (docs/windows-compatibility.md): the
# verified FEX source, the ARM FEX toolchain and its x86 SDK (fex-payload.sh builds the payload
# from them), and the tooling image Wine is rebuilt in (wine.sh). They hold no secrets and push
# nothing; each image goes into an OCI archive with its digest (oci-build.sh), which the
# publishing checks push with that digest unchanged.
#
# usage: windows-compat.sh fex-source ARCHIVE
#        windows-compat.sh wine-tools OUT
#        windows-compat.sh fex-buildroot OUT
#        windows-compat.sh fex-x86-sdk OUT
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

cmd=${1:?usage: windows-compat.sh fex-source|wine-tools|fex-buildroot|fex-x86-sdk ...}
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
  *) die "unknown command $cmd" ;;
esac
