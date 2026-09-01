#!/usr/bin/env bash
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 debian|fedora|arch" >&2; exit 2; }
distro=$1

case "$distro" in
debian)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        git curl ca-certificates build-essential clang libclang-dev cmake \
        pkg-config capnproto file dpkg-dev \
        libxkbcommon-dev libwayland-dev libseat-dev libinput-dev libudev-dev \
        libdrm-dev libgbm-dev libgl-dev libegl-dev libpipewire-0.3-dev \
        libdbus-1-dev libssl-dev libfontconfig-dev libfreetype-dev \
        libharfbuzz-dev libfuse3-dev libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev
    ;;
fedora)
    dnf install -y \
        git curl gcc gcc-c++ make clang clang-devel cmake perl \
        pkgconf-pkg-config capnproto file rpm-build rpmlint \
        libxkbcommon-devel wayland-devel libseat-devel libinput-devel \
        systemd-devel libdrm-devel mesa-libgbm-devel mesa-libGL-devel \
        mesa-libEGL-devel pipewire-devel dbus-devel openssl-devel \
        fontconfig-devel freetype-devel harfbuzz-devel fuse3-devel \
        gstreamer1-devel gstreamer1-plugins-base-devel
    dnf clean all
    ;;
arch)
    pacman -Syu --noconfirm \
        git curl base-devel rustup clang cmake pkgconf capnproto file namcap \
        libxkbcommon wayland seatd libinput systemd-libs libdrm mesa \
        pipewire dbus openssl fontconfig freetype2 harfbuzz fuse3 \
        gstreamer gst-plugins-base-libs
    ;;
*)
    echo "unknown distro: $distro" >&2
    exit 2
    ;;
esac

if [[ $distro != arch ]]; then
    export RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
    export PATH=/usr/local/cargo/bin:$PATH
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal
    if [[ -n ${GITHUB_PATH:-} ]]; then
        echo /usr/local/cargo/bin >> "$GITHUB_PATH"
    fi
fi

rustup show active-toolchain
cargo --version
