#!/usr/bin/env bash
# The CI builder image (os/Containerfile.builder) that t's checks and the CI jobs run in, with
# the toolchain rust-toolchain.toml pins baked in.
#
#   ci-builder.sh build ARCH OUT   build it natively into OUT/image.oci.tar (buildah image)
#   ci-builder.sh inputs           the files it is built from, which date it (source-date.sh)
# Its canaries are tools/ci/publish/ci-builder-canary.sh, kept apart so that changing them
# doesn't rebuild the image.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/build-lib.sh"
cd "$repo_root"

# buildah bind-mounts single files only from a writable context, so the build gets a copy of
# exactly the files the Containerfile uses.
context_files=(dist/ci/install-build-deps.sh os/external-assets/github-assets.tsv
  tools/ci/check-build-env.sh tools/ci/install-android-ndk.sh tools/ci/install-pdfium.sh
  tools/ci/native-mold-linker.sh tools/ci/verify-native-mold.sh tools/ci/windows/wine-prefix.sh
  tools/ci/install-wine.sh os/external-assets/wine/*.patch
  tools/ci/install-cargo.sh os/external-assets/cargo/*.patch
  tools/ci/shstk-mark.py os/scripts/verify-elf-hardening.sh
  tools/font-catalog.py crates/sdk/ui-core/assets/fonts/manifest.toml
  crates/sdk/ui-core/assets/fonts/artifacts.json)

case ${1:-} in
  build)
    arch=${2:?usage: ci-builder.sh build ARCH OUT}
    out=${3:?}
    case $arch in
      x86_64) platform=linux/amd64 pdfium=pdfium-8057-linux-x64.tgz
              extra=(innosetup-6.7.3.exe jsign-7.6.jar "$(bash tools/ci/install-wine.sh --asset-name)") ;;
      aarch64) platform=linux/arm64 pdfium=pdfium-8057-linux-arm64.tgz extra=() ;;
      *) die "unknown architecture $arch" ;;
    esac
    [[ $(uname -m) == "$arch" ]] || die "the $arch builder is built natively, not on $(uname -m)"
    channel=$(awk -F'"' '/^channel/{print $2}' rust-toolchain.toml)
    [[ -n $channel ]] || die 'no channel in rust-toolchain.toml'
    assets=$(pinned_assets "sccache-v0.17.0-$arch-unknown-linux-musl.tar.gz" \
      "wasm-bindgen-0.2.108-$arch-unknown-linux-musl.tar.gz" \
      "cargo-xwin-v0.23.1.$arch-unknown-linux-musl.tar.gz" \
      "mold-2.40.4-$arch-linux.tar.gz" "$pdfium" \
      "$(bash tools/ci/install-cargo.sh --asset-name)" \
      "${extra[@]}")
    # Every pinned font, laid out by the font-seed check, which the calling check needs.
    font_seed=/target/font-seed
    context=$scratch/ci-builder-context
    rm -rf "$context"
    for file in "${context_files[@]}"; do
      install -D -m 0644 "$file" "$context/$file"
    done
    bash tools/ci/publish/oci-build.sh "$context" os/Containerfile.builder "$out" \
      --platform "$platform" --build-context "github-assets=$assets" \
      --build-context "font-seed=$font_seed" \
      --build-arg "RUST_CHANNEL=$channel"
    ;;
  inputs)
    # What the image is built from, beside oci-build.sh and build-lib.sh: this script, the
    # Containerfile, the toolchain's channel, what the build copies and its patch directories
    # whole, the pinned fonts' manifests (in the context files), and the pinned assets' manifest, which names (and cache-github-assets.sh fetches into
    # os-assets) every external asset the build takes. A file the build starts reading belongs
    # here too, or a change to it would leave the image's date where it was.
    printf '%s\n' tools/ci/publish/ci-builder.sh os/Containerfile.builder rust-toolchain.toml \
      os/dev/cache-github-assets.sh os/external-assets/wine \
      os/external-assets/cargo \
      "${context_files[@]}" | sort -u
    ;;
  *) die 'usage: ci-builder.sh build ARCH OUT | ci-builder.sh inputs' ;;
esac
