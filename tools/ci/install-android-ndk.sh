#!/usr/bin/env bash
# Install the pinned Android NDK and the Rust targets of the Android app host
# (docs/android.md) into an x86_64 Linux build environment.
#
#   install-android-ndk.sh
#
# os/Containerfile.builder runs this at image build time; check.yml's `android`
# job runs it again so builders published before the NDK joined the recipe
# still work. An NDK whose recorded digest matches the pin is kept, so on a
# current builder only the rustup target check remains. The Rust toolchain is
# TILO_RUST_TOOLCHAIN when set, otherwise the one rust-toolchain.toml selects.
#
# The release and digest match dist/dev/Containerfile.fedora44, the developer
# image, whose flat build context cannot include this script.
set -euo pipefail

release=r29
sha256=4abbbcdc842f3d4879206e9695d52709603e52dd68d3c1fff04b3b5e7a308ecf
prefix=/opt/android-ndk

if [ "$(uname -m)" != x86_64 ]; then
    echo "install-android-ndk: NDK $release publishes Linux host tools for x86_64 only" >&2
    exit 1
fi

if [ "$(cat "$prefix/.tilo-ndk-sha256" 2>/dev/null || true)" != "$sha256" ]; then
    if ! command -v unzip >/dev/null; then
        dnf install -y --setopt=install_weak_deps=False unzip
    fi
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    curl --proto '=https' --tlsv1.2 -fsSL --retry 5 -o "$work/ndk.zip" \
        "https://dl.google.com/android/repository/android-ndk-${release}-linux.zip"
    echo "$sha256  $work/ndk.zip" | sha256sum -c -
    unzip -q "$work/ndk.zip" -d "$work"
    rm -rf "$prefix"
    mv "$work/android-ndk-$release" "$prefix"
    echo "$sha256" > "$prefix/.tilo-ndk-sha256"
fi

toolchain=()
if [ -n "${TILO_RUST_TOOLCHAIN:-}" ]; then
    toolchain=(--toolchain "$TILO_RUST_TOOLCHAIN")
fi
rustup target add "${toolchain[@]}" aarch64-linux-android x86_64-linux-android
