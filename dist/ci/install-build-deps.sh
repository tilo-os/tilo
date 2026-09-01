#!/usr/bin/env bash
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 debian|fedora|arch" >&2; exit 2; }
distro=$1

case "$distro" in
debian)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        curl ca-certificates build-essential clang libclang-dev cmake \
        pkg-config capnproto file dpkg-dev \
        libxkbcommon-dev libwayland-dev libseat-dev libinput-dev libudev-dev \
        libdrm-dev libgbm-dev libgl-dev libegl-dev libpipewire-0.3-dev \
        libdbus-1-dev libssl-dev libfontconfig-dev libfreetype-dev \
        libharfbuzz-dev libfuse3-dev libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev
    ;;
fedora)
    dnf install -y --setopt=install_weak_deps=False \
        curl gcc gcc-c++ make clang clang-devel cmake perl \
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
        curl git base-devel rustup clang cmake pkgconf capnproto file namcap \
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
    rustup_version=1.29.0
    case "$(uname -m)" in
    x86_64)
        rustup_target=x86_64-unknown-linux-gnu
        rustup_sha=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
        ;;
    aarch64)
        rustup_target=aarch64-unknown-linux-gnu
        rustup_sha=9732d6c5e2a098d3521fca8145d826ae0aaa067ef2385ead08e6feac88fa5792
        ;;
    *)
        echo "unsupported Rust bootstrap architecture: $(uname -m)" >&2
        exit 1
        ;;
    esac
    rustup_init=/usr/local/bin/rustup-init
    curl --proto '=https' --tlsv1.2 -fsSL \
        "https://static.rust-lang.org/rustup/archive/$rustup_version/$rustup_target/rustup-init" \
        -o "$rustup_init"
    printf '%s  %s\n' "$rustup_sha" "$rustup_init" | sha256sum --check --status
    chmod 0755 "$rustup_init"
    # The shell installer stages rustup-init under /tmp and refuses to run it
    # in the PRoot userspace after its executable-access probe. Install the
    # checksum-pinned binary under /usr/local and execute it directly; PRoot's
    # configured QEMU handler supplies ARM emulation transparently.
    "$rustup_init" -y --no-modify-path --profile minimal
fi

rustup show active-toolchain
cargo --version
