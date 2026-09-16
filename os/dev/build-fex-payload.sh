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
test -d /x86-sdk/usr/include
test -s /usr/lib/qos/fex-buildroot-packages.txt
export PATH=/usr/lib64/llvm21/bin:/usr/local/bin:$PATH
export QOS_FEX_X86_SYSROOT=/x86-sdk
export CCACHE_DISABLE=1
# Read-only named build contexts can retain their source UID. This exception is
# confined to this disposable process; no host Git configuration is changed.
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*'
mkdir -p /stage/usr/lib/qos/fex
bash /qos/os/dev/fetch-fex-source.sh /src > /stage/usr/lib/qos/fex/source.txt
export SOURCE_DATE_EPOCH
SOURCE_DATE_EPOCH=$(git -C /src show -s --format=%ct HEAD)
cmake -S /src -B /build -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    -DCMAKE_INSTALL_LIBDIR=lib64 \
    -DCMAKE_C_COMPILER=/usr/lib64/llvm21/bin/clang \
    -DCMAKE_CXX_COMPILER=/usr/lib64/llvm21/bin/clang++ \
    -DClang_DIR=/usr/lib64/llvm21/lib64/cmake/clang \
    -DLLVM_DIR=/usr/lib64/llvm21/lib64/cmake/llvm \
    -DBUILD_THUNKS=ON -DBUILD_FEX_LINUX_TESTS=ON -DBUILD_TESTING=ON \
    -DENABLE_CLANG_THUNKS=ON -DENABLE_CCACHE=OFF -DENABLE_LTO=ON \
    -DTUNE_CPU=none -DTUNE_ARCH=generic \
    -DX86_DEV_ROOTFS=/x86-sdk \
    -DX86_64_TOOLCHAIN_FILE=/qos/os/dev/toolchains/fex-cross-x86_64.cmake \
    -DX86_32_TOOLCHAIN_FILE=/qos/os/dev/toolchains/fex-cross-i686.cmake
cmake --build /build --parallel "${BUILD_JOBS:-2}"
for project in /build /build/Guest /build/Guest_32; do
    DESTDIR=/stage cmake --install "$project"
done
# Preserve FEX and bundled dependency license/notice files with source paths.
python3 - <<'PY'
from pathlib import Path
import shutil
source = Path('/src')
destination = Path('/stage/usr/share/licenses/qos-fex')
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
cp /usr/lib/qos/fex-buildroot-packages.txt /stage/usr/lib/qos/fex/buildroot-packages.txt
cp /build/CMakeCache.txt /stage/usr/lib/qos/fex/cmake-cache.txt
printf 'FEX_BUILDROOT_REF=%s\nFEX_X86_SDK_REF=%s\n' \
    "$FEX_BUILDROOT_REF" "$FEX_X86_SDK_REF" > /stage/usr/lib/qos/fex/inputs.env
python3 /qos/os/dev/validate-fex-payload.py /stage \
    > /stage/usr/lib/qos/fex/payload-inventory.json
