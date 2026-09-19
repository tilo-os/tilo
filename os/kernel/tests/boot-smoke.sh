#!/usr/bin/env bash
# Native, diskless kernel boot and module-signature acceptance test.
# This does not establish UEFI, installer, or physical-board acceptance.
set -euo pipefail
stage=$(realpath "${1:?usage: boot-smoke.sh STAGE.tar.gz KERNEL_RELEASE OUTPUT_DIRECTORY}")
release=${2:?kernel release required}
out=${3:?new output directory required}
case "$release" in ''|*[!a-zA-Z0-9._+-]*) echo 'invalid kernel release' >&2; exit 2;; esac
case "$(uname -m)" in
    aarch64) qemu=qemu-system-aarch64; machine=virt,gic-version=3; console=ttyAMA0;;
    x86_64) qemu=qemu-system-x86_64; machine=q35; console=ttyS0;;
    *) echo 'run on a native aarch64 or x86_64 Linux builder' >&2; exit 2;;
esac
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# Preserve old evidence and refuse to reuse a root containing stale test files.
mkdir "$out"
out=$(realpath "$out")
mkdir "$out/rootfs"
gcc -O2 -Wall -Wextra -Werror "$script_dir/boot-init.c" -o "$out/rootfs/init"
ldd "$out/rootfs/init" > "$out/init-dependencies.txt"
while read -r dependency; do
    mkdir -p "$out/rootfs$(dirname "$dependency")"
    cp -L "$dependency" "$out/rootfs$dependency"
done < <(awk '/=> \// { print $3 }' "$out/init-dependencies.txt")
interpreter=$(readelf -l "$out/rootfs/init" | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
test -n "$interpreter"
mkdir -p "$out/rootfs$(dirname "$interpreter")"
cp -L "$interpreter" "$out/rootfs$interpreter"
python3 "$script_dir/prepare-boot-smoke.py" "$stage" "$release" "$out"
(cd "$out/rootfs"; find . -print0 | cpio --null -o --format=newc --quiet | gzip -1 > "$out/initramfs.cpio.gz")
timeout 180 "$qemu" -machine "$machine" -cpu max -accel tcg -smp 2 -m 1024 \
    -nographic -no-reboot -nic none -kernel "$out/Image" -initrd "$out/initramfs.cpio.gz" \
    -append "console=$console rdinit=/init panic=1" > "$out/serial.log" 2>&1
grep -F "TILO_KERNEL_USERSPACE_OK release=$release machine=$(uname -m)" "$out/serial.log"
grep -F 'TILO_MODULE_SIGNATURE_ENFORCEMENT_OK' "$out/serial.log"
if grep -Eq 'TILO_.*FAILED|Kernel panic' "$out/serial.log"; then
    echo 'kernel acceptance failed; inspect serial.log' >&2
    exit 1
fi
printf 'PASS: native kernel userspace and module-signature enforcement (%s)\n' "$out"
