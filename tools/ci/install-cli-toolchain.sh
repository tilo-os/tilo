#!/usr/bin/env bash
# The C toolchain the `tilo` command's Linux release archives need, installed into the image the
# cli-{arch} check's setup makes:
#
#   install-cli-toolchain.sh ARCH   what builds ARCH-unknown-linux-musl on this machine
#
# The program links C on both sides of its dependency tree (ring through rustls, SQLite through
# rusqlite), so every Linux build needs a C compiler that compiles against musl's headers: the
# distribution's glibc headers are a different C library, and the program is one static file that
# asks the kernel for everything.
#
# ARCH is this machine's architecture: the distribution's musl-gcc is exactly that, a gcc wrapper
# that compiles against its musl and links statically.
#
# ARCH is aarch64 on the x86_64 machine the cli checks run on: distributions ship no cross
# musl-gcc, so this installs Zig, whose shipped compiler carries musl's headers and libc for every
# architecture and does both the compiling and the linking of the cross build
# (tools/ci/publish/cli-release.sh). It also installs qemu's user-mode aarch64 emulator, which runs
# the cross-built program's `--help` here rather than only reading its ELF header.
set -euo pipefail

case ${1:-} in
  x86_64) arch=x86_64 ;;
  aarch64) arch=aarch64 ;;
  *) echo 'usage: install-cli-toolchain.sh ARCH (x86_64 or aarch64)' >&2; exit 2 ;;
esac
machine=$(uname -m)

if [[ $arch == "$machine" ]]; then
  dnf install -y --setopt=install_weak_deps=False musl-gcc
  dnf clean all
  command -v musl-gcc >/dev/null
  exit 0
fi

[[ $machine == x86_64 && $arch == aarch64 ]] \
  || { echo "no toolchain builds $arch on $machine" >&2; exit 1; }

# Pinned by version and checksum, not by a moving download.
zig_version=0.15.2
zig_asset=zig-x86_64-linux-$zig_version.tar.xz
zig_sha256=02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
curl -fsSL --retry 3 -o "$stage/$zig_asset" "https://ziglang.org/download/$zig_version/$zig_asset"
printf '%s  %s\n' "$zig_sha256" "$stage/$zig_asset" | sha256sum --check --strict -
mkdir -p "/opt/zig-$zig_version"
tar -xf "$stage/$zig_asset" --strip-components=1 --no-same-owner -C "/opt/zig-$zig_version"
install -d /usr/local/bin
ln -sf "/opt/zig-$zig_version/zig" /usr/local/bin/zig
zig version

# The program's own architecture, emulated: `--help` runs where the build runs, so the cross
# archive is checked the way the native one is.
dnf install -y --setopt=install_weak_deps=False "qemu-user-static-$arch"
dnf clean all
command -v "qemu-$arch-static" >/dev/null

# The target's standard library, which the release build compiles against, for the toolchain
# rust-toolchain.toml pins: it runs where that file is.
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
rustup target add "$arch-unknown-linux-musl"
rustup target list --installed | grep -qx "$arch-unknown-linux-musl"
