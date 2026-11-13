#!/usr/bin/env bash
# The C toolchain the `tilo` command's release archives need, installed into the image the
# cli-{arch} and cli-macos-cross-{arch} checks' setup makes:
#
#   install-cli-toolchain.sh TARGET   what builds TARGET on this x86_64 Linux machine
#
# TARGET is a build the release carries: x86_64-unknown-linux-musl, aarch64-unknown-linux-musl,
# aarch64-apple-darwin or x86_64-apple-darwin.
#
# The program links C on both sides of its dependency tree (ring through rustls, SQLite through
# rusqlite), so every build needs a C compiler that compiles against the C library of its target:
# the distribution's glibc headers are a different C library, and the Linux program is one static
# file that asks the kernel for everything.
#
# Zig provides that compiler for every target but the machine's own. Its shipped compiler carries
# musl's headers and libc for every architecture, and Darwin's C library headers, so it compiles
# and links the program for a target this machine is not. It is pinned by version and checksum,
# not by a moving download.
#
# The aarch64 Linux build also needs qemu's user-mode emulator, which runs the cross-built
# program's `--help` here rather than only reading its ELF header. A Mac's program cannot run
# here at all, and the check reads its Mach-O headers instead.
set -euo pipefail

case ${1:-} in
  x86_64-unknown-linux-musl | aarch64-unknown-linux-musl | aarch64-apple-darwin | x86_64-apple-darwin)
    target=$1
    ;;
  *)
    echo 'usage: install-cli-toolchain.sh TARGET (x86_64-unknown-linux-musl,' \
      'aarch64-unknown-linux-musl, aarch64-apple-darwin or x86_64-apple-darwin)' >&2
    exit 2
    ;;
esac
arch=${target%%-*}
machine=$(uname -m)

# The packages this needs from Fedora, from the release repository of the image's own version
# alone, at one host: a released repository never changes (its updates go to another one), and dnf
# checks every package's signature and the checksum its metadata gives, so the same image gets
# the same packages on every run, and the checks' setup is served from t's cache (t.toml).
install_released() {
  dnf install -y --setopt=install_weak_deps=False --repo=fedora --setopt=fedora.metalink= \
    '--setopt=fedora.baseurl=https://dl.fedoraproject.org/pub/fedora/linux/releases/$releasever/Everything/$basearch/os/' \
    "$@"
  dnf clean all
}

install_zig() {
  local zig_version=0.15.2
  local zig_asset=zig-x86_64-linux-$zig_version.tar.xz
  local zig_sha256=02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239
  local stage
  if command -v zig >/dev/null && [[ $(zig version) == "$zig_version" ]]; then return; fi
  stage=$(mktemp -d)
  trap 'rm -rf -- "$stage"' EXIT
  curl -fsSL --retry 3 -o "$stage/$zig_asset" "https://ziglang.org/download/$zig_version/$zig_asset"
  printf '%s  %s\n' "$zig_sha256" "$stage/$zig_asset" | sha256sum --check --strict -
  mkdir -p "/opt/zig-$zig_version"
  tar -xf "$stage/$zig_asset" --strip-components=1 --no-same-owner -C "/opt/zig-$zig_version"
  install -d /usr/local/bin
  ln -sf "/opt/zig-$zig_version/zig" /usr/local/bin/zig
  rm -rf -- "$stage"
  trap - EXIT
  zig version
}

# The target's standard library, which the release build compiles against, for the toolchain
# rust-toolchain.toml pins: it runs where that file is.
add_rust_target() {
  cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
  rustup target add "$target"
  rustup target list --installed | grep -qx "$target"
}

case $target in
  *-linux-musl)
    if [[ $arch == "$machine" ]]; then
      # The distribution's musl-gcc, for the machine's own architecture: a gcc wrapper that
      # compiles against its musl and links statically. Its Rust target is the image's.
      install_released musl-gcc
      command -v musl-gcc >/dev/null
      exit 0
    fi
    [[ $machine == x86_64 && $arch == aarch64 ]] \
      || { echo "no toolchain builds $target on $machine" >&2; exit 1; }
    install_zig
    # The program's own architecture, emulated: `--help` runs where the build runs, so the cross
    # archive is checked the way the native one is.
    install_released "qemu-user-static-$arch"
    command -v "qemu-$arch-static" >/dev/null
    add_rust_target
    ;;
  *-apple-darwin)
    # No Mac is a build node: the program for one is built here, by Zig, against link stubs of the
    # frameworks it uses (tools/ci/macos-frameworks.py). That needs no Apple SDK, which is Apple's
    # to license on Apple hardware only, and would not run here if it had one.
    [[ $machine == x86_64 ]] || { echo "no toolchain builds $target on $machine" >&2; exit 1; }
    install_zig
    add_rust_target
    # The check reads the Mach-O the build leaves (tools/ci/publish/cli-release.sh); llvm-otool
    # and llvm-objdump read it here, as a Mac's otool would there.
    if ! command -v llvm-otool >/dev/null || ! command -v llvm-objdump >/dev/null; then
      install_released llvm
    fi
    command -v llvm-otool >/dev/null
    command -v llvm-objdump >/dev/null
    ;;
esac
