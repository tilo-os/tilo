#!/usr/bin/env bash
# Native Clang emits Windows objects; LLVM's PE linker uses MinGW import libs.
# Usage: CLANG=clang LLD_LINK=/path/to/rust-lld bash this-script OUTPUT GUEST_SDK
set -euo pipefail
output=${1:?output directory required}
sdk=${2:?guest SDK root containing usr/*-w64-mingw32/sys-root/mingw/lib required}
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
compiler=${CLANG:-clang}
linker=("${LLD_LINK:-lld-link}")
if [[ "${linker[0]##*/}" = rust-lld ]]; then
    linker+=(-flavor link)
fi
mkdir -p "$output"
for bits in 32 64; do
    case "$bits" in
        32) triple=i686-w64-mingw32; machine=x86 ;;
        64) triple=x86_64-w64-mingw32; machine=x64 ;;
    esac
    imports="$sdk/usr/$triple/sys-root/mingw/lib/libkernel32.a"
    test -s "$imports"
    "$compiler" --target="$triple" -O2 -ffreestanding -fno-stack-protector \
        -fno-builtin -Wall -Wextra -Werror -c "$source_dir/fixtures/windows-dxvk-load.c" \
        -o "$output/dxvk-load-$bits.obj"
    "${linker[@]}" /machine:"$machine" /entry:mainCRTStartup /subsystem:console \
        /nodefaultlib /timestamp:0 /safeseh:no /out:"$output/dxvk-load-$bits.exe" \
        "$output/dxvk-load-$bits.obj" "$imports"
    sha256sum "$output/dxvk-load-$bits.exe"
done
