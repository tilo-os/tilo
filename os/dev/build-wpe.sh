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
# Staged installs: each package's .pc file keeps advertising the *unstaged*
# ${prefix} (/usr), while the files are under $DESTDIR_ROOT — so the next
# package in the stack has to be told where they really are. Have pkgconf
# derive ${prefix} from each .pc file's own location instead
# (= `--define-prefix`), which relocates exactly the packages that were staged
# and leaves everything else alone.
#
# NOT PKG_CONFIG_SYSROOT_DIR: that is a blanket rewrite of every -I/-L
# pkg-config emits, including system libraries in the real /usr. With
# DESTDIR=/wpe it turned libxkbcommon's -I/usr/include into -I/wpe/usr/include
# and libwpe's configure died on `Imported target "XkbCommon::libxkbcommon"
# includes non-existent path`.
export PKG_CONFIG_RELOCATE_PATHS=1
# Pin real compilers: meson otherwise auto-wraps with sccache, whose shared
# daemon may run outside the build container and not see its /usr/include.
export CC=gcc CXX=g++

# Assert a just-installed package resolves to include paths that exist. A
# misresolved .pc otherwise surfaces much later as a confusing compile error
# inside whichever package consumed it.
check_pc() {
    local dir
    dir=$(pkg-config --cflags-only-I "$1" | tr ' ' '\n' | grep -m1 '^-I' | cut -c3-) || true
    if [ -z "$dir" ] || [ ! -d "$dir" ]; then
        echo "pkg-config $1 resolved to a missing include dir: '${dir:-<none>}'" >&2
        exit 1
    fi
    echo "$1 -> $dir"
}

# Run a build step, keeping the log short on success and showing enough of it
# to diagnose on failure. Piping straight to `tail -1` hides the compiler
# error behind ninja's parting "build stopped: subcommand failed".
STEP_LOG=$CACHE/step.log
step() { # step <lines-to-show-when-it-works> <command...>
    local keep=$1
    shift
    if ! "$@" >"$STEP_LOG" 2>&1; then
        echo "FAILED: $* — last 80 lines:" >&2
        tail -80 "$STEP_LOG" >&2
        exit 1
    fi
    tail -"$keep" "$STEP_LOG"
}

echo "=== libwpe ==="
cd "$CACHE/${LIBWPE%.tar.xz}"
step 1 cmake -B build -GNinja -Wno-dev -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_INSTALL_LIBDIR=lib64
step 1 env DESTDIR="$DESTDIR_ROOT" ninja -C build install
check_pc wpe-1.0

echo "=== wpebackend-fdo ==="
cd "$CACHE/${FDO%.tar.xz}"
rm -rf build
step 1 meson setup build --prefix="$PREFIX" --libdir lib64 --buildtype=release
step 1 env DESTDIR="$DESTDIR_ROOT" ninja -C build install
check_pc wpebackend-fdo-1.0

echo "=== wpewebkit (long) ==="
cd "$CACHE/${WPEWK%.tar.xz}"
# USE_LIBBACKTRACE=OFF: Fedora ships no standalone libbacktrace (its own
# webkitgtk builds without it).
step 8 cmake -B build -GNinja -Wno-dev -DPORT=WPE \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$PREFIX" \
    -DCMAKE_INSTALL_LIBDIR=lib64 \
    -DENABLE_DOCUMENTATION=OFF -DENABLE_INTROSPECTION=OFF \
    -DENABLE_MINIBROWSER=ON -DENABLE_WPE_PLATFORM=ON \
    -DUSE_LIBBACKTRACE=OFF
step 5 ninja -C build -j"$JOBS"
step 1 env DESTDIR="$DESTDIR_ROOT" ninja -C build install
echo WPE_BUILD_OK
