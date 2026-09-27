#!/usr/bin/env bash
# Build the WPE WebKit stack (libwpe + wpebackend-fdo + wpewebkit) from
# release tarballs. Tilo uses WPE WebKit as the engine behind the ui
# toolkit's WebView widget; Fedora does not package WPE, so we build it —
# with this script for development, and inside os/Containerfile.wpe's
# `wpe-builder-1` stage.
#
# Usage:
#   build-wpe.sh deps    install build dependencies (dnf, needs sudo)
#   build-wpe.sh verify-sources  verify pinned archives without compiling
#   build-wpe.sh build   download + compile + install into the prefix
#
# Locations (override via env):
#   TILO_WPE_CACHE    tarballs + build trees   (default ~/.cache/tilo-wpe)
#   TILO_WPE_PREFIX   configured install prefix (default $TILO_WPE_CACHE/prefix;
#                    the OS image build uses /usr)
#   TILO_WPE_DESTDIR  staging root prepended at install time (default empty;
#                    the OS image build stages into /wpe for COPY --from)
#   JOBS             ninja parallelism for the wpewebkit build (default 8;
#                    WebKit compile jobs are memory-hungry, ~2 GB each)
set -euo pipefail

CACHE=${TILO_WPE_CACHE:-$HOME/.cache/tilo-wpe}
PREFIX=${TILO_WPE_PREFIX:-$CACHE/prefix}
DESTDIR_ROOT=${TILO_WPE_DESTDIR:-}
REL=https://wpewebkit.org/releases
JOBS=${JOBS:-8}
case "$JOBS" in
    ''|*[!0-9]*|0*) echo "JOBS must be a positive integer without leading zeros" >&2; exit 2 ;;
esac
SOURCE_DIR=${TILO_WPE_SOURCE_DIR:-}
OFFLINE=${TILO_WPE_OFFLINE:-0}
MODE=${1:-build}

LIBWPE=libwpe-1.16.3.tar.xz
LIBWPE_SHA256=c880fa8d607b2aa6eadde7d6d6302b1396ebc38368fe2332fa20e193c7ee1420
FDO=wpebackend-fdo-1.16.1.tar.xz
FDO_SHA256=544ae14012f8e7e426b8cb522eb0aaaac831ad7c35601d1cf31d37670e0ebb3b
WPEWK=wpewebkit-2.52.6.tar.xz
WPEWK_SHA256=b2bafef2751625b7fdf530f230ff0f542ff0eeba3590c3a989d931b2a55c858e

if [ "$MODE" = deps ]; then
    # wpewebkit builds from the same tree as webkitgtk, so Fedora's
    # webkitgtk6.0 build deps cover almost everything.
    sudo dnf -y builddep webkitgtk6.0 2>&1 | tail -3
    # What the WPE port needs and the GTK port does not, so builddep misses it:
    # libinput + libudev (systemd-devel) back ENABLE_WPE_PLATFORM_DRM, and
    # gbm/wayland/epoxy the rest of WPEPlatform. Keep in sync with the
    # snapshot buildroot in os/Containerfile.wpe-buildroot.
    sudo dnf -y install meson ninja-build gperf unifdef libgbm-devel \
        wayland-devel wayland-protocols-devel libepoxy-devel \
        libinput-devel systemd-devel 2>&1 | tail -2
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

echo "tarballs: $LIBWPE $FDO $WPEWK"

prepare_source() {
    local t="$1" expected="$2" source archive part actual
    source=${SOURCE_DIR:+$SOURCE_DIR/$t}
    archive=$CACHE/$t
    if [ -n "$source" ] && [ -f "$source" ]; then
        archive=$source
    elif [ ! -f "$archive" ]; then
        [ "$OFFLINE" != 1 ] || {
            echo "missing offline WPE source: $t" >&2
            exit 1
        }
        part=$archive.part
        curl --proto '=https' --tlsv1.2 --location --fail --show-error \
            --retry 8 --retry-all-errors --retry-max-time 180 \
            --output "$part" "$REL/$t"
        mv -f -- "$part" "$archive"
    fi
    actual=$(sha256sum "$archive" | cut -d ' ' -f 1)
    [ "$actual" = "$expected" ] || {
        echo "WPE source checksum mismatch for $t: expected $expected, got $actual" >&2
        exit 1
    }
    printf '%s\n' "$archive"
}

for spec in \
    "$LIBWPE:$LIBWPE_SHA256" \
    "$FDO:$FDO_SHA256" \
    "$WPEWK:$WPEWK_SHA256"; do
    t=${spec%%:*}
    expected=${spec#*:}
    archive=$(prepare_source "$t" "$expected")
    d=${t%.tar.xz}
    if [ "$MODE" != verify-sources ]; then
        [ -d "$d" ] || tar xf "$archive"
    fi
done

if [ "$MODE" = verify-sources ]; then
    echo SOURCES_OK
    exit 0
fi
[ "$MODE" = build ] || { echo "usage: $0 deps|verify-sources|build" >&2; exit 2; }

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

STEP_LOG=$CACHE/step.log

# Assert a just-installed package's .pc points at things that exist: an
# include directory, and a library the linker can actually find. Both halves
# have been wrong here, and each surfaced much later as a confusing error
# inside whichever package consumed it.
check_pc() {
    local inc
    inc=$(pkg-config --cflags-only-I "$1" | tr ' ' '\n' | grep -m1 '^-I' | cut -c3-) || true
    if [ -z "$inc" ] || [ ! -d "$inc" ]; then
        echo "pkg-config $1: include dir '${inc:-<none>}' does not exist" >&2
        exit 1
    fi
    # Word splitting on the pkg-config output is the point here.
    # shellcheck disable=SC2046
    if ! echo 'int main(void){return 0;}' |
        $CC -x c - -o /dev/null $(pkg-config --cflags --libs "$1") 2>"$STEP_LOG"; then
        echo "pkg-config $1: nothing links against it —" >&2
        tail -5 "$STEP_LOG" >&2
        exit 1
    fi
    echo "$1 -> $inc (links)"
}

# Run a build step, keeping the log short on success and showing enough of it
# to diagnose on failure. Piping straight to `tail -1` hides the compiler
# error behind ninja's parting "build stopped: subcommand failed".
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
step 1 env DESTDIR="$DESTDIR_ROOT" ninja -C build -j"$JOBS" install
# libwpe's wpe.pc.in hardcodes `libdir=${exec_prefix}/lib` while the library
# itself installs into CMAKE_INSTALL_LIBDIR — lib64 on Fedora — so as shipped
# the .pc sends the linker to a directory that contains no libwpe. Nothing
# notices in an unstaged install, because the linker finds lib64 on its
# default search path anyway; a staging root has no default path to fall back
# on, and wpebackend-fdo fails with `cannot find -lwpe-1.0`. Correct the .pc
# rather than aliasing lib -> lib64 inside the staging root: that symlink gets
# copied into the image and replaces the real /usr/lib.
sed -i 's|^libdir=.*|libdir=${exec_prefix}/lib64|' \
    "$DESTDIR_ROOT$PREFIX/lib64/pkgconfig/wpe-1.0.pc"
check_pc wpe-1.0

echo "=== wpebackend-fdo ==="
cd "$CACHE/${FDO%.tar.xz}"
rm -rf build
step 1 meson setup build --prefix="$PREFIX" --libdir lib64 --buildtype=release
step 1 env DESTDIR="$DESTDIR_ROOT" ninja -C build -j"$JOBS" install
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
# Not `step`, which buffers: this compile runs the better part of an hour, and
# a step emitting nothing for that long is indistinguishable from a hung one —
# to whoever is reading CI, and to any inactivity watchdog between here and the
# runner. Stream a line periodically while still keeping the full log for the
# failure path.
set +e
ninja -C build -j"$JOBS" 2>&1 | tee "$STEP_LOG" |
    awk 'NR % 500 == 0 { print "  ninja: " $0; fflush() }'
ninja_rc=${PIPESTATUS[0]}
set -e
if [ "$ninja_rc" -ne 0 ]; then
    echo "FAILED: wpewebkit compile — last 80 lines:" >&2
    tail -80 "$STEP_LOG" >&2
    exit 1
fi
tail -1 "$STEP_LOG"

step 1 env DESTDIR="$DESTDIR_ROOT" ninja -C build -j"$JOBS" install
echo WPE_BUILD_OK
