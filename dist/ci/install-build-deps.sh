#!/usr/bin/env bash
set -euo pipefail

# The single list of native build dependencies. `DISTRO` alone installs what a
# desktop-session package build needs. `fedora workspace` adds what building and
# checking the whole Cargo workspace needs on top; the developer distrobox
# (dist/dev/Containerfile.fedora44) and the CI builder (os/Containerfile.builder)
# both install exactly that, so the two environments cannot drift.
#
# `fedora test` adds, on top of `workspace`, what running the whole test suite
# needs: the runtime libraries, plugins, fonts, software Vulkan driver and test
# subjects the suites drive, plus the pinned cargo-nextest. The developer
# distrobox installs it so the suite runs there as it does in CI; the CI builder
# keeps its own workflow tooling.
#
# TILO_RUST_TOOLCHAIN (default: stable) and TILO_RUST_COMPONENTS (space
# separated) select what the checksum-pinned rustup bootstrap installs.
usage() { echo "usage: $0 debian|fedora|arch | $0 fedora workspace|test" >&2; exit 2; }
[[ $# == 1 || $# == 2 ]] || usage
distro=$1
profile=${2:-package}
case "$distro:$profile" in
*:package|fedora:workspace|fedora:test) ;;
*) usage ;;
esac

# The suites' runtime, matching what os/Containerfile.builder gives CI jobs.
# Weak dependencies stay enabled so runtime plugins and drivers arrive.
# vulkan-loader + mesa-vulkan-drivers: lavapipe/llvmpipe, so GPU tests and
# goldens render headless on the CPU. GStreamer plugins: the media suites encode
# and decode real VP9, AV1 and H.264/AAC (dav1d is the AV1 decoder the
# real-time stack prefers). The Noto set: text shaping and golden rendering.
# gstreamer1-plugin-openh264 pulls Cisco's openh264 from Fedora's
# fedora-cisco-openh264 repository, downloaded here by whoever builds this
# environment, as Cisco's licence requires: the video-editor suites
# (tests/support/generated_clip.rs and src/preview.rs) encode their H.264
# sample clips with openh264enc. It belongs only in these CI and developer
# environments (os/Containerfile.builder, dist/dev/Containerfile.fedora44),
# never in a Tilo image: those install from the frozen RPM snapshot, which
# os/scripts/verify-no-cisco-openh264.sh keeps free of it.
# pulseaudio: only the private software endpoint tools/with-headless-audio.sh
# starts; it replaces PipeWire's PulseAudio shim where that is installed.
# bubblewrap, GNOME Text Editor, cryptsetup, swtpm and tpm2-tools are test
# subjects that their suites refuse to skip. libsecret (secret-tool) and
# gnome-keyring are the Secret Service a session keeps its MLS keys in: a
# headless session runs one, unlocked, so its apps' channels open as a
# person's do. python3-gobject gives tilo-secret-service's suite libsecret's
# own client (gi Secret-1) to drive the service with, as apps would.
# Xwayland and xwayland-satellite are the X11 bridge the shell's X11 clipboard
# and focus tests drive through the compositor. Satellite itself is built from
# pinned source with Tilo's patch by tools/ci/install-xwayland-satellite.sh once
# Rust is installed; this installs its build headers and the font and cursor
# library it needs at runtime.
# pipewire, wireplumber and pipewire-gstreamer: a private PipeWire with a
# session manager, which the screen-capture tests start to serve a headless
# screen node to the production pipewiresrc capture path; pipewire-utils'
# pw-dump finds the node they create.
install_test_runtime_fedora() {
    if rpm -q pipewire-pulseaudio >/dev/null 2>&1; then
        dnf swap -y pipewire-pulseaudio pulseaudio
    fi
    dnf install -y \
        pulseaudio \
        gstreamer1 gstreamer1-plugins-good gstreamer1-plugins-bad-free \
        gstreamer1-plugin-dav1d gstreamer1-plugin-openh264 \
        google-noto-sans-fonts google-noto-sans-mono-fonts \
        google-noto-serif-fonts google-noto-color-emoji-fonts \
        google-noto-sans-symbols-fonts google-noto-sans-symbols-2-fonts \
        google-noto-sans-math-fonts google-noto-sans-cjk-fonts \
        google-noto-sans-arabic-fonts google-noto-sans-hebrew-fonts \
        google-noto-sans-devanagari-fonts google-noto-sans-bengali-fonts \
        google-noto-sans-thai-fonts \
        vulkan-loader mesa-vulkan-drivers \
        bubblewrap gnome-text-editor \
        cryptsetup swtpm swtpm-tools tpm2-tools \
        libsecret gnome-keyring python3-gobject \
        xorg-x11-server-Xwayland xcb-util-cursor-devel open-sans-fonts \
        pipewire wireplumber pipewire-gstreamer pipewire-utils
    # Package scriptlets that create system accounts (sysusers) or compile
    # GSettings schemas can be skipped or fail inside unprivileged image
    # builds without failing the transaction. Replay both idempotently so the
    # image never depends on whether they ran.
    systemd-sysusers
    getent passwd pulse >/dev/null || {
        echo 'pulseaudio system account is missing after sysusers' >&2
        exit 1
    }
    glib-compile-schemas /usr/share/glib-2.0/schemas
    test -s /usr/share/glib-2.0/schemas/gschemas.compiled
    install_nextest
}

# The test runner, pinned and checksum-verified. It goes to /usr/local/bin, not
# CARGO_HOME, so it stays on PATH when a build tool mounts its own CARGO_HOME.
install_nextest() {
    local version=0.9.146 target sha archive
    case "$(uname -m)" in
    x86_64)
        target=x86_64-unknown-linux-gnu
        sha=682c21b777c333e96fd532e114d3a5a894e0729ab88d94c0a9f20f8419695428
        ;;
    aarch64)
        target=aarch64-unknown-linux-gnu
        sha=b2e33d7c72de7ade0ff7b3a948ac37516b24f8a836b7a8870c1f634a94be9de9
        ;;
    *)
        echo "unsupported cargo-nextest architecture: $(uname -m)" >&2
        exit 1
        ;;
    esac
    archive=$(mktemp)
    curl --proto '=https' --tlsv1.2 -fsSL \
        "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$version/cargo-nextest-$version-$target.tar.gz" \
        -o "$archive"
    printf '%s  %s\n' "$sha" "$archive" | sha256sum --check --status
    tar -xzf "$archive" -C /usr/local/bin cargo-nextest
    rm -f -- "$archive"
    chmod 0755 /usr/local/bin/cargo-nextest
    /usr/local/bin/cargo-nextest nextest --version
}

case "$distro" in
debian)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        curl ca-certificates build-essential binutils clang libclang-dev cmake \
        pkg-config capnproto file dpkg-dev autoconf automake libtool libgcrypt20-dev \
        libxkbcommon-dev libwayland-dev libseat-dev libinput-dev libudev-dev \
        libdrm-dev libgbm-dev libgl-dev libegl-dev libpipewire-0.3-dev \
        libdbus-1-dev dbus-daemon xdg-dbus-proxy libfontconfig-dev libfreetype-dev \
        libharfbuzz-dev libfuse3-dev libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev
    ;;
fedora)
    dnf install -y --setopt=install_weak_deps=False \
        curl git jq gcc gcc-c++ binutils make clang clang-devel cmake perl \
        pkgconf-pkg-config capnproto file rpm-build rpm-sign rpmlint \
        autoconf automake libtool libgcrypt-devel \
        libxkbcommon-devel wayland-devel libseat-devel libinput-devel \
        systemd-devel libdrm-devel mesa-libgbm-devel mesa-libGL-devel \
        mesa-libEGL-devel pipewire-devel dbus-devel dbus-daemon xdg-dbus-proxy \
        fontconfig-devel freetype-devel harfbuzz-devel fuse3-devel \
        gstreamer1-devel gstreamer1-plugins-base-devel
    if [[ $profile == workspace || $profile == test ]]; then
        # nasm, perl-FindBin and python3 drive vendored native builds;
        # zlib-ng-compat provides zlib. Video goes through GStreamer, so no
        # FFmpeg or libva development packages are needed. bash-devel and
        # readline-devel are what Terminal's Bash editor builds against
        # (crates/runtime/ush/native/build.py), as in os/Containerfile.
        dnf install -y --setopt=install_weak_deps=False \
            nasm perl-FindBin python3 \
            sqlite-devel zlib-ng-compat-devel expat-devel \
            bash-devel readline-devel
    fi
    if [[ $profile == test ]]; then
        install_test_runtime_fedora
    fi
    dnf clean all
    ;;
arch)
    packages=(
        curl git base-devel binutils rustup clang cmake pkgconf capnproto file namcap
        libxkbcommon wayland seatd libinput systemd-libs libdrm mesa
        pipewire dbus xdg-dbus-proxy fontconfig freetype2 harfbuzz fuse3 libgcrypt
        gstreamer gst-plugins-base-libs
    )
    # Under QEMU/PRoot, libalpm's faccessat probe cannot reopen sync databases
    # that were seeded by the host even though ordinary guest stat calls see
    # them. Refresh, upgrade, resolve, and install in one pacman transaction;
    # the redirected database/cache live on explicit host-backed PRoot binds,
    # so pacman's atomic downloads persist for the whole transaction.
    if [[ $(uname -m) == aarch64 ]]; then
        pacman -Syu --needed --noconfirm "${packages[@]}"
    else
        pacman -Syu --noconfirm
        pacman -S --needed --noconfirm "${packages[@]}"
    fi
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
    rustup_args=(-y --no-modify-path --profile minimal
        --default-toolchain "${TILO_RUST_TOOLCHAIN:-stable}")
    for component in ${TILO_RUST_COMPONENTS:-}; do
        rustup_args+=(--component "$component")
    done
    "$rustup_init" "${rustup_args[@]}"
fi

rustup show active-toolchain
cargo --version
