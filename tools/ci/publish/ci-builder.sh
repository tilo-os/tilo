#!/usr/bin/env bash
# The CI builder image (os/Containerfile.builder) that t's checks and the CI jobs run in, with
# the toolchain rust-toolchain.toml pins baked in.
#
#   ci-builder.sh build ARCH OUT   build it natively into OUT/image.oci.tar (buildah image)
# Its canaries are tools/ci/publish/ci-builder-canary.sh, kept apart so that changing them
# doesn't rebuild the image.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

case ${1:-} in
  build)
    arch=${2:?usage: ci-builder.sh build ARCH OUT}
    out=${3:?}
    case $arch in
      x86_64) platform=linux/amd64 pdfium=pdfium-8057-linux-x64.tgz
              extra=(innosetup-6.7.3.exe jsign-7.6.jar) ;;
      aarch64) platform=linux/arm64 pdfium=pdfium-8057-linux-arm64.tgz extra=() ;;
      *) die "unknown architecture $arch" ;;
    esac
    [[ $(uname -m) == "$arch" ]] || die "the $arch builder is built natively, not on $(uname -m)"
    channel=$(awk -F'"' '/^channel/{print $2}' rust-toolchain.toml)
    [[ -n $channel ]] || die 'no channel in rust-toolchain.toml'
    assets=$(pinned_assets "sccache-v0.17.0-$arch-unknown-linux-musl.tar.gz" \
      "wasm-bindgen-0.2.108-$arch-unknown-linux-musl.tar.gz" \
      "cargo-xwin-v0.23.1.$arch-unknown-linux-musl.tar.gz" \
      "mold-2.40.4-$arch-linux.tar.gz" "$pdfium" "$(bash tools/ci/install-xwayland-satellite.sh --asset-name)" \
      "${extra[@]}")
    # buildah bind-mounts single files only from a writable context, so the build gets a copy
    # of exactly the files the Containerfile uses.
    context=$scratch/ci-builder-context
    rm -rf "$context"
    for file in dist/ci/install-build-deps.sh os/external-assets/github-assets.tsv \
      tools/ci/check-build-env.sh tools/ci/install-android-ndk.sh tools/ci/install-pdfium.sh \
      tools/ci/native-mold-linker.sh tools/ci/verify-native-mold.sh tools/ci/windows/wine-prefix.sh \
      tools/ci/install-xwayland-satellite.sh os/external-assets/xwayland-satellite/*.patch \
      tools/ci/shstk-mark.py os/scripts/verify-elf-hardening.sh; do
      install -D -m 0644 "$file" "$context/$file"
    done
    bash tools/ci/publish/oci-build.sh "$context" os/Containerfile.builder "$out" \
      --platform "$platform" --build-context "github-assets=$assets" \
      --build-arg "RUST_CHANNEL=$channel"
    ;;
  *) die 'usage: ci-builder.sh build ARCH OUT' ;;
esac
