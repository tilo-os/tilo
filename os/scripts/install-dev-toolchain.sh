#!/bin/sh
# The dev image's compiler and build packages, in os/Containerfile.dev.
#
#   install-dev-toolchain.sh rust
#       copy the Rust release rust-toolchain.toml pins out of the
#       RUST_TOOLCHAIN_IMAGE mounted at /run/tilo-rust-toolchains into /usr
#   install-dev-toolchain.sh packages [EXTRA...]
#       install os/scripts/builder-packages.txt (the production builder's
#       packages but for its Rust compiler) and EXTRA from the frozen repo at
#       /repo, at the fixed build clock
#   install-dev-toolchain.sh tools-tree DEST
#       the `tilo-dev-tools` sysext tree: the pinned Rust release (as `rust`,
#       but under DEST) plus the third-party GUI clients the VM acceptance suite
#       opens, as the files of exactly the packages they add on top of the
#       image this runs in. Only /usr and /opt travel: a systemd-sysext cannot
#       carry /etc or /var. DEST is then a tar-able layer for tilo-vm.
#
# The toolchain lives under /usr so it updates with the image (/usr/local is
# /var/usrlocal on bootc, which an image update never touches), linked into
# /usr/bin rather than behind rustup proxies: tilo-vmd runs in-VM builds with
# systemd's PATH, and without rustup nothing reinterprets rust-toolchain.toml.
set -eu
dev=/run/tilo-dev

case "${1:-}" in
  rust)
    channel="$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$dev/rust-toolchain.toml")"
    [ -n "$channel" ]
    toolchain="$channel-$(uname -m)-unknown-linux-gnu"
    if [ ! -x "/run/tilo-rust-toolchains/$toolchain/bin/rustc" ]; then
      printf 'RUST_TOOLCHAIN_IMAGE has no %s toolchain, which rust-toolchain.toml pins (it has: %s).\n' \
        "$toolchain" "$(ls /run/tilo-rust-toolchains | tr '\n' ' ')" >&2
      printf 'Pin a tilo-builder index built from this rust-toolchain.toml (build-builder-image.yml tags it %s).\n' \
        "$channel" >&2
      exit 1
    fi
    cp -a "/run/tilo-rust-toolchains/$toolchain" /usr/lib/rust-toolchain
    for tool in /usr/lib/rust-toolchain/bin/*; do ln -s "$tool" "/usr/bin/${tool##*/}"; done
    actual="$(rustc --version | awk '{print $2}')"
    if [ "$actual" != "$channel" ]; then
      printf 'the dev image compiles with rustc %s, but rust-toolchain.toml pins %s\n' "$actual" "$channel" >&2
      exit 1
    fi
    cargo --version
    ;;
  packages)
    shift
    list=/run/tilo-dev-builder-packages
    grep -v '^#' "$dev/os/scripts/builder-packages.txt" > "$list"
    [ -s "$list" ]
    if [ "$#" -gt 0 ]; then
      LD_PRELOAD=/run/tilo-build-clock.so dnf install -y --setopt=reposdir=/repo "$@"
    fi
    xargs -r -a "$list" env LD_PRELOAD=/run/tilo-build-clock.so dnf install -y --setopt=reposdir=/repo
    rm -f "$list"
    dnf clean all
    if [ -x /usr/libexec/tilo/finalize-rpmdb ]; then /usr/libexec/tilo/finalize-rpmdb; fi
    rm -rf /var/log/dnf5.log* /var/cache/libdnf5 /var/lib/dnf/system-repo.lock /run/dnf /etc/ld.so.cache
    rustc --version && cargo --version && sccache --version
    ;;
  tools-tree)
    dest="${2:?tools-tree needs a destination directory}"
    channel="$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$dev/rust-toolchain.toml")"
    [ -n "$channel" ]
    toolchain="$channel-$(uname -m)-unknown-linux-gnu"
    [ -x "/run/tilo-rust-toolchains/$toolchain/bin/rustc" ] \
      || { printf 'RUST_TOOLCHAIN_IMAGE has no %s toolchain\n' "$toolchain" >&2; exit 1; }
    mkdir -p "$dest/usr/lib" "$dest/usr/bin"
    cp -a "/run/tilo-rust-toolchains/$toolchain" "$dest/usr/lib/rust-toolchain"
    for tool in "$dest"/usr/lib/rust-toolchain/bin/*; do
      ln -s "/usr/lib/rust-toolchain/bin/${tool##*/}" "$dest/usr/bin/${tool##*/}"
    done
    query='%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}\n'
    rpm -qa --qf "$query" | sort > /run/tilo-dev-before
    LD_PRELOAD=/run/tilo-build-clock.so dnf install -y --setopt=reposdir=/repo \
      --setopt=install_weak_deps=False \
      chromium gnome-text-editor featherpad qt6-qtwayland alacritty glx-utils
    rpm -qa --qf "$query" | sort > /run/tilo-dev-after
    comm -13 /run/tilo-dev-before /run/tilo-dev-after > /run/tilo-dev-added
    [ -s /run/tilo-dev-added ]
    xargs -r -a /run/tilo-dev-added rpm -ql | sort -u > /run/tilo-dev-files
    # Directories are recreated by cp --parents; files outside /usr and /opt
    # (default /etc config, /var state) cannot be part of a sysext.
    grep -E '^/(usr|opt)/' /run/tilo-dev-files | while IFS= read -r file; do
      if [ -d "$file" ] && [ ! -L "$file" ]; then continue; fi
      cp -a --parents "$file" "$dest"
    done
    grep -Ev '^/(usr|opt)/' /run/tilo-dev-files | sed 's/^/not in the sysext: /' >&2 || true
    rm -f /run/tilo-dev-before /run/tilo-dev-after /run/tilo-dev-added /run/tilo-dev-files
    ;;
  *)
    echo 'usage: install-dev-toolchain.sh rust | packages [EXTRA...] | tools-tree DEST' >&2
    exit 2
    ;;
esac
