#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Cross-link only: signed ARM64 musl-dev sysroot, no execution or installation.
set -euo pipefail
if [ "$#" -ne 2 ]; then
    echo 'Usage: build_arm64_entry.sh SIGNED_ARM64_SYSROOT NEW_ENTRY' >&2
    exit 2
fi
sysroot=$(realpath "$1")
output=$2
if [ -e "$output" ]; then
    echo 'Refusing existing entry output' >&2
    exit 2
fi
tools_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
builtins=("$sysroot"/usr/lib/gcc/aarch64-alpine-linux-musl/*/libgcc.a)
if [ "${#builtins[@]}" -ne 1 ] || [ ! -f "${builtins[0]}" ]; then
    echo 'Signed ARM64 gcc development input is required for musl compiler builtins' >&2
    exit 2
fi
clang --target=aarch64-linux-musl --sysroot="$sysroot" -fuse-ld=lld \
    -std=c11 -O2 -Wall -Wextra -Werror -static -nostdlib \
    "$sysroot/usr/lib/crt1.o" "$sysroot/usr/lib/crti.o" \
    "$tools_dir/atl_entry.c" -L"$sysroot/usr/lib" \
    -Wl,--start-group -lc "${builtins[0]}" -Wl,--end-group \
    "$sysroot/usr/lib/crtn.o" -o "$output"
sha256sum "$output"
