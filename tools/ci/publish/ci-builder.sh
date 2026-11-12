#!/usr/bin/env bash
# The CI builder image (os/Containerfile.builder) that t's checks and the CI jobs run in, with
# the toolchain rust-toolchain.toml pins baked in.
#
#   ci-builder.sh build ARCH OUT   build it natively into OUT/image.oci.tar (buildah image)
#   ci-builder.sh assets ARCH      the pinned external assets (github-assets.tsv) it takes
#   ci-builder.sh context DIR      lay out in DIR exactly the files the Containerfile reads
#   ci-builder.sh inputs           the files it is built from, which date it (source-date.sh)
#   ci-builder.sh tag              the registry tag of the image these inputs build
# Its canaries are tools/ci/publish/ci-builder-canary.sh, kept apart so that changing them
# doesn't rebuild the image. .forgejo/workflows/build-ci-builder.yml publishes it under its tag,
# and tools/ci/resolve_builder.py runs every CI job in the image its commit's inputs build, so
# no job runs in an image older than what its commit asks for.
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

assets() {
  local arch=$1 pdfium extra=()
  case $arch in
    x86_64) pdfium=pdfium-8057-linux-x64.tgz
            extra=(innosetup-6.7.3.exe jsign-7.6.jar "$(bash tools/ci/install-wine.sh --asset-name)") ;;
    aarch64) pdfium=pdfium-8057-linux-arm64.tgz ;;
    *) die "unknown architecture $arch" ;;
  esac
  printf '%s\n' "sccache-v0.17.0-$arch-unknown-linux-musl.tar.gz" \
    "wasm-bindgen-0.2.108-$arch-unknown-linux-musl.tar.gz" \
    "cargo-xwin-v0.23.1.$arch-unknown-linux-musl.tar.gz" \
    "mold-2.40.4-$arch-linux.tar.gz" "$pdfium" \
    "$(bash tools/ci/install-cargo.sh --asset-name)" "${extra[@]}"
}

context() {
  local dir=$1 file
  rm -rf "$dir"
  for file in "${context_files[@]}"; do
    install -D -m 0644 "$file" "$dir/$file"
  done
}

# What the image is built from, beside oci-build.sh and build-lib.sh: this script, the
# Containerfile, the toolchain's channel, what the build copies and its patch directories whole,
# the pinned fonts' manifests (in the context files), and the pinned assets' manifest, which
# names (and cache-github-assets.sh fetches into os-assets) every external asset the build
# takes. A file the build starts reading belongs here too, or a change to it would leave the
# image's date and tag where they were.
inputs() {
  printf '%s\n' tools/ci/publish/ci-builder.sh os/Containerfile.builder rust-toolchain.toml \
    os/dev/cache-github-assets.sh os/external-assets/wine \
    os/external-assets/cargo \
    "${context_files[@]}" | sort -u
}

case ${1:-} in
  build)
    arch=${2:?usage: ci-builder.sh build ARCH OUT}
    out=${3:?}
    case $arch in
      x86_64) platform=linux/amd64 ;;
      aarch64) platform=linux/arm64 ;;
      *) die "unknown architecture $arch" ;;
    esac
    [[ $(uname -m) == "$arch" ]] || die "the $arch builder is built natively, not on $(uname -m)"
    channel=$(awk -F'"' '/^channel/{print $2}' rust-toolchain.toml)
    [[ -n $channel ]] || die 'no channel in rust-toolchain.toml'
    mapfile -t names < <(assets "$arch")
    asset_dir=$(pinned_assets "${names[@]}")
    # Every pinned font, laid out by the font-seed check, which the calling check needs.
    font_seed=/target/font-seed
    context "$scratch/ci-builder-context"
    bash tools/ci/publish/oci-build.sh "$scratch/ci-builder-context" os/Containerfile.builder "$out" \
      --platform "$platform" --build-context "github-assets=$asset_dir" \
      --build-context "font-seed=$font_seed" \
      --build-arg "RUST_CHANNEL=$channel"
    ;;
  assets) assets "${2:?usage: ci-builder.sh assets ARCH}" ;;
  context) context "${2:?usage: ci-builder.sh context DIR}" ;;
  inputs) inputs ;;
  tag)
    # The Git object ids of every input, so a change to any of them names a new image. Only
    # trees are read, which a blobless or sparse checkout has.
    mapfile -t paths < <(inputs)
    printf 'inputs-%s\n' "$(git ls-tree -r --full-tree HEAD -- "${paths[@]}" | sha256sum | cut -c1-32)"
    ;;
  *) die 'usage: ci-builder.sh build ARCH OUT | assets ARCH | context DIR | inputs | tag' ;;
esac
