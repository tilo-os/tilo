#!/usr/bin/env bash
# Fixed layout used by Containerfile.fex-payload; all input resolution is external.
set -euo pipefail
for reference in "${FEX_BUILDROOT_REF:?}" "${FEX_X86_SDK_REF:?}"; do
    if [[ ! "$reference" =~ ^[^[:space:]@]+@sha256:[0-9a-f]{64}$ ]]; then
        echo 'FEX payload inputs must use full immutable OCI digest references' >&2
        exit 1
    fi
done
[[ "${BUILD_JOBS:-2}" =~ ^[1-9][0-9]*$ ]]
test "$(uname -m)" = aarch64
test ! -e /stage
test ! -e /build
test ! -e /source
test -d /x86-sdk/usr/include
test -s /usr/lib/tilo/fex-buildroot-packages.txt
export PATH=/usr/lib64/llvm21/bin:/usr/local/bin:$PATH
export TILO_FEX_X86_SYSROOT=/x86-sdk
export CCACHE_DISABLE=1
# Probe the same Clang/GCC-sysroot/lld path used by both guest CMake projects
# before compiling the native FEX tree. The foreign binaries are link-checked,
# not executed on ARM.
printf '#include <string>\nint main() { return std::string("x").size() != 1; }\n' \
    > /tmp/tilo-fex-cross-smoke.cpp
for target in x86_64-linux-gnu i686-linux-gnu; do
    /usr/bin/clang++ --target="$target" --gcc-toolchain=/x86-sdk/usr \
        --sysroot=/x86-sdk -fuse-ld=lld /tmp/tilo-fex-cross-smoke.cpp \
        -o "/tmp/tilo-fex-cross-smoke-$target"
    test -s "/tmp/tilo-fex-cross-smoke-$target"
    rm "/tmp/tilo-fex-cross-smoke-$target"
done
rm /tmp/tilo-fex-cross-smoke.cpp
# Read-only named build contexts can retain their source UID. This exception is
# confined to this disposable process; no host Git configuration is changed.
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*'
mkdir -p /stage/usr/lib/tilo/fex
# Never modify the caller's read-only upstream checkout. Verify the actual
# disposable copy before applying reviewed, hash-pinned downstream changes.
mkdir /source
cp -a /src/. /source/
bash /tilo/os/dev/fetch-fex-source.sh /source > /stage/usr/lib/tilo/fex/source.txt
python3 /tilo/os/dev/apply-fex-patches.py /source /tilo/os/external-assets/fex-patches \
    > /stage/usr/lib/tilo/fex/downstream-patches.json
# The guest CMake projects use these architecture-specific pkg-config roots.
# Check every requested graphics/Wayland module before building native FEX.
for target in x86_64-linux-gnu i686-linux-gnu; do
    case "$target" in
        x86_64-linux-gnu) libdir=lib64 ;;
        i686-linux-gnu) libdir=lib ;;
    esac
    export PKG_CONFIG_SYSROOT_DIR=/x86-sdk
    export PKG_CONFIG_LIBDIR="/x86-sdk/usr/$libdir/pkgconfig:/x86-sdk/usr/lib/$target/pkgconfig:/x86-sdk/usr/share/pkgconfig"
    pkg-config --print-errors --exists xcb x11 xrandr xrender wayland-client gl egl
    if [[ "$target" == x86_64-linux-gnu ]]; then
        pkg-config --print-errors --exists alsa libdrm
    fi
done
cat > /tmp/tilo-fex-vulkan-headers.cpp <<'EOF'
#define VK_USE_64_BIT_PTR_DEFINES 0
#define VK_USE_PLATFORM_XLIB_XRANDR_EXT
#define VK_USE_PLATFORM_XLIB_KHR
#define VK_USE_PLATFORM_XCB_KHR
#define VK_USE_PLATFORM_WAYLAND_KHR
#include <vulkan/vulkan.h>
EOF
for target in x86_64-linux-gnu i686-linux-gnu; do
    /usr/bin/clang++ --target="$target" --gcc-toolchain=/x86-sdk/usr \
        --sysroot=/x86-sdk -I/source/External/Vulkan-Headers/include \
        -std=c++20 -fsyntax-only /tmp/tilo-fex-vulkan-headers.cpp
done
rm /tmp/tilo-fex-vulkan-headers.cpp
echo 'FEX x86 guest pkg-config modules and Vulkan platform headers verified'
unset PKG_CONFIG_SYSROOT_DIR PKG_CONFIG_LIBDIR
mkdir -p /stage/usr/share/tilo/fex
cp -a /tilo/os/external-assets/fex-patches /stage/usr/share/tilo/fex/patches
export SOURCE_DATE_EPOCH
SOURCE_DATE_EPOCH=$(git -C /source show -s --format=%ct HEAD)
cmake -S /source -B /build -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    -DCMAKE_INSTALL_LIBDIR=lib64 \
    -DCMAKE_C_COMPILER=/usr/lib64/llvm21/bin/clang \
    -DCMAKE_CXX_COMPILER=/usr/lib64/llvm21/bin/clang++ \
    -DClang_DIR=/usr/lib64/llvm21/lib64/cmake/clang \
    -DLLVM_DIR=/usr/lib64/llvm21/lib64/cmake/llvm \
    -DBUILD_THUNKS=ON -DBUILD_FEX_LINUX_TESTS=ON -DBUILD_TESTING=ON \
    -DBUILD_FEXCONFIG=OFF \
    -DENABLE_CLANG_THUNKS=ON -DENABLE_CCACHE=OFF -DENABLE_LTO=ON \
    -DTUNE_CPU=none -DTUNE_ARCH=generic \
    -DX86_DEV_ROOTFS=/x86-sdk \
    -DX86_64_TOOLCHAIN_FILE=/tilo/os/dev/toolchains/fex-cross-x86_64.cmake \
    -DX86_32_TOOLCHAIN_FILE=/tilo/os/dev/toolchains/fex-cross-i686.cmake
cmake --build /build --parallel "${BUILD_JOBS:-2}"
for project in /build /build/Guest /build/Guest_32; do
    DESTDIR=/stage cmake --install "$project"
done
# Preserve FEX and bundled dependency license/notice files with source paths.
python3 - <<'PY'
from pathlib import Path
import shutil
source = Path('/source')
destination = Path('/stage/usr/share/licenses/tilo-fex')
for item in source.rglob('*'):
    relative = item.relative_to(source)
    if '.git' in relative.parts or not item.is_file() or item.is_symlink():
        continue
    name = item.name.upper()
    if name.startswith(('LICENSE', 'LICENCE', 'COPYING', 'NOTICE', 'COPYRIGHT')):
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(item, target)
PY
cp /usr/lib/tilo/fex-buildroot-packages.txt /stage/usr/lib/tilo/fex/buildroot-packages.txt
cp /build/CMakeCache.txt /stage/usr/lib/tilo/fex/cmake-cache.txt
printf 'FEX_BUILDROOT_REF=%s\nFEX_X86_SDK_REF=%s\n' \
    "$FEX_BUILDROOT_REF" "$FEX_X86_SDK_REF" > /stage/usr/lib/tilo/fex/inputs.env
python3 /tilo/os/dev/validate-fex-payload.py /stage \
    > /stage/usr/lib/tilo/fex/payload-inventory.json
