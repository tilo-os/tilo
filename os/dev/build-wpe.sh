#!/usr/bin/env bash
# Build the WPE WebKit stack (libwpe + wpebackend-fdo + wpewebkit) from
# release tarballs. qOS uses WPE WebKit as the engine behind the ui
# toolkit's WebView widget; Fedora does not package WPE, so we build it —
# with this script for development, and inside the OS image's
# `wpe-builder` Containerfile stage.
#
# Usage:
#   build-wpe.sh deps    install build dependencies (dnf, needs sudo)
#   build-wpe.sh build   download + compile + install into the prefix
#
# Locations (override via env):
#   QOS_WPE_CACHE    tarballs + build trees   (default ~/.cache/qos-wpe)
#   QOS_WPE_PREFIX   configured install prefix (default $QOS_WPE_CACHE/prefix;
#                    the OS image build uses /usr)
#   QOS_WPE_DESTDIR  staging root prepended at install time (default empty;
#                    the OS image build stages into /wpe for COPY --from)
#   JOBS             ninja parallelism for the wpewebkit build (default 8;
#                    WebKit compile jobs are memory-hungry, ~2 GB each)
set -euo pipefail

CACHE=${QOS_WPE_CACHE:-$HOME/.cache/qos-wpe}
PREFIX=${QOS_WPE_PREFIX:-$CACHE/prefix}
DESTDIR_ROOT=${QOS_WPE_DESTDIR:-}
REL=https://wpewebkit.org/releases
JOBS=${JOBS:-8}

if [ "${1:-build}" = deps ]; then
    # wpewebkit builds from the same tree as webkitgtk, so Fedora's
    # webkitgtk6.0 build deps cover almost everything.
    sudo dnf -y builddep webkitgtk6.0 2>&1 | tail -3
    sudo dnf -y install meson ninja-build gperf unifdef libgbm-devel \
        wayland-devel wayland-protocols-devel libepoxy-devel 2>&1 | tail -2
    echo DEPS_OK
    exit 0
fi

mkdir -p "$CACHE" "$DESTDIR_ROOT$PREFIX/lib64"
# For non-system prefixes, alias lib -> lib64 so a .pc file that says
# ${prefix}/lib still resolves. NEVER for /usr: staging a symlink there
# and copying it into an image would clobber the real /usr/lib.
if [ "$PREFIX" != /usr ] && [ ! -e "$DESTDIR_ROOT$PREFIX/lib" ]; then
    ln -s lib64 "$DESTDIR_ROOT$PREFIX/lib"
fi

cd "$CACHE"

latest() { # latest tarball name for package $1 matching version regex $2
    curl -s "$REL/" | grep -oE "$1-$2\.tar\.xz" | sort -uV | tail -1
}

LIBWPE=$(latest libwpe '1\.[0-9]+\.[0-9]+')
FDO=$(latest wpebackend-fdo '1\.[0-9]+\.[0-9]+')
WPEWK=$(latest wpewebkit '2\.52\.[0-9]+')
echo "tarballs: $LIBWPE $FDO $WPEWK"

for t in "$LIBWPE" "$FDO" "$WPEWK"; do
    [ -f "$t" ] || curl -sLO "$REL/$t"
    d=${t%.tar.xz}
    [ -d "$d" ] || tar xf "$t"
done

export PKG_CONFIG_PATH="$DESTDIR_ROOT$PREFIX/lib64/pkgconfig:$DESTDIR_ROOT$PREFIX/share/pkgconfig:${PKG_CONFIG_PATH:-}"
export LD_LIBRARY_PATH="$DESTDIR_ROOT$PREFIX/lib64:${LD_LIBRARY_PATH:-}"
# Staged installs: make pkg-config resolve the .pc files' unstaged
# ${prefix} paths inside the staging root.
[ -n "$DESTDIR_ROOT" ] && export PKG_CONFIG_SYSROOT_DIR="$DESTDIR_ROOT"
# Pin real compilers: meson otherwise auto-wraps with sccache, whose shared
# daemon may run outside the build container and not see its /usr/include.
export CC=gcc CXX=g++

echo "=== libwpe ==="
cd "$CACHE/${LIBWPE%.tar.xz}"
# Drop a stale CMake cache so a reconfigure (e.g. after the base image /
# toolchain changed) regenerates cleanly instead of failing on paths that
# moved; compiled objects survive for an incremental ninja.
rm -f build/CMakeCache.txt; rm -rf build/CMakeFiles
cmake -B build -GNinja -Wno-dev -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_INSTALL_LIBDIR=lib64 >/dev/null
DESTDIR="$DESTDIR_ROOT" ninja -C build install | tail -1

echo "=== wpebackend-fdo ==="
cd "$CACHE/${FDO%.tar.xz}"
rm -rf build
meson setup build --prefix="$PREFIX" --libdir lib64 --buildtype=release >/dev/null
DESTDIR="$DESTDIR_ROOT" ninja -C build install 2>&1 | tail -1

echo "=== wpewebkit (long) ==="
cd "$CACHE/${WPEWK%.tar.xz}"
# Heal a stale CMake cache (see libwpe above) — WebKit is where a base /
# toolchain change surfaced "Build files cannot be regenerated correctly".
rm -f build/CMakeCache.txt; rm -rf build/CMakeFiles
# USE_LIBBACKTRACE=OFF: Fedora ships no standalone libbacktrace (its own
# webkitgtk builds without it).
cmake -B build -GNinja -Wno-dev -DPORT=WPE \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$PREFIX" \
    -DCMAKE_INSTALL_LIBDIR=lib64 \
    -DENABLE_DOCUMENTATION=OFF -DENABLE_INTROSPECTION=OFF \
    -DENABLE_MINIBROWSER=ON -DENABLE_WPE_PLATFORM=ON \
    -DUSE_LIBBACKTRACE=OFF \
    2>&1 | tail -8
ninja -C build -j"$JOBS" 2>&1 | tail -5
DESTDIR="$DESTDIR_ROOT" ninja -C build install 2>&1 | tail -1
echo WPE_BUILD_OK
