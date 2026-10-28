#!/usr/bin/env bash
# The compiler cache the kernel compiles through (image-kernel-cache-<arch>):
#
#   kernel-compile-cache.sh ARCH OUT
#
# every object of the kernel and its modules, compiled into an empty ccache directory and packed
# as OUT/compiler-cache.tar (buildah, as root in a micro-VM step). It reads the buildroot and
# what kernel-env.sh sets up, not kernel.sh, whose publishing steps change no object.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
. "$(dirname -- "${BASH_SOURCE[0]}")/kernel-env.sh"
cd "$repo_root"
arch=${1:-}
out=${2:-}
[[ -n $arch && -n $out ]] || die 'usage: kernel-compile-cache.sh ARCH OUT'
kernel_build_setup "$arch"
mkdir -p "$out"
started=$SECONDS
# ccache stores each entry compressed already.
cache=$scratch/compiler-cache
rm -rf "$cache" && mkdir -p "$cache"
# Keyed by the buildroot alone, it compiles whatever the commit's patches and configuration
# make, and a check or compile they fail leaves it with what compiled: the kernel build, keyed
# by them, is what enforces them.
b build --layers --timestamp 0 --file os/kernel/Containerfile --target compile-cache \
  "${args[@]}" --build-arg TILO_KERNEL_CHECKS=advisory --volume "$cache:/work/compiler-cache" .
# The same entries make the same archive: image-kernel is keyed by these bytes, and a cache
# compiled again under the same key (another node, an evicted result) must not rebuild the kernel
# for times, owners or the order the files were listed in.
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  --pax-option=exthdr.name=%d/PaxHeaders/%f,delete=atime,delete=ctime \
  -cf "$out/compiler-cache.tar" -C "$cache" .
printf 'compiler cache of the %s kernel: %s in %ss\n' "$arch" \
  "$(du -sh "$out/compiler-cache.tar" | cut -f1)" "$((SECONDS - started))"
