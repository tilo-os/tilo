#!/usr/bin/env bash
# Run on the launcher's native build architecture. No disk image is attached.
set -euo pipefail
if [[ $# != 3 ]]; then
    echo "Usage: $0 KERNEL_IMAGE TILO_LAUNCHER NEW_OUTPUT_DIRECTORY" >&2
    exit 2
fi
kernel=$(realpath "$1")
launcher=$(realpath "$2")
test -f "$kernel"
test -x "$launcher"
case "$(uname -m)" in
    aarch64) qemu=(qemu-system-aarch64 -machine virt,gic-version=3 -cpu max); console=ttyAMA0 ;;
    x86_64) qemu=(qemu-system-x86_64 -machine q35 -cpu max); console=ttyS0 ;;
    *) echo "Unsupported native test architecture" >&2; exit 2 ;;
esac
fixture=$(dirname "$(realpath "$0")")/fixtures/landlock-private-tmp.c
mkdir "$3"
out=$(realpath "$3")
rootfs=$out/rootfs
mkdir -p "$rootfs"/{usr/bin,proc,dev,tmp,denied,lib,lib64}
cp "$launcher" "$rootfs/usr/libexec/tilo/tilo-launcher"
"${CC:-cc}" -O2 -Wall -Wextra -Werror "$fixture" -o "$rootfs/init"
cp "$rootfs/init" "$rootfs/usr/bin/tmp-probe"
for binary in "$rootfs/init" "$rootfs/usr/libexec/tilo/tilo-launcher"; do
    ldd "$binary" > "$out/dependencies.txt"
    if grep -q 'not found' "$out/dependencies.txt"; then
        cat "$out/dependencies.txt" >&2
        exit 1
    fi
    while read -r dependency; do
        mkdir -p "$rootfs$(dirname "$dependency")"
        cp -L "$dependency" "$rootfs$dependency"
    done < <(awk '/=> \// { print $3 } /^[[:space:]]*\// { print $1 }' "$out/dependencies.txt")
    interpreter=$(readelf -l "$binary" | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
    test -n "$interpreter"
    mkdir -p "$rootfs$(dirname "$interpreter")"
    cp -L "$interpreter" "$rootfs$interpreter"
done
(cd "$rootfs"; find . -print0 | cpio --null -o --format=newc --quiet | gzip -1 > "$out/initramfs.cpio.gz")
sha256sum "$kernel" "$launcher" > "$out/inputs.sha256"
status=0
timeout --kill-after=10s 180 "${qemu[@]}" -accel tcg -smp 2 -m 1024 \
    -nographic -no-reboot -nic none -kernel "$kernel" -initrd "$out/initramfs.cpio.gz" \
    -append "console=$console rdinit=/init panic=1" > "$out/serial.log" 2>&1 || status=$?
printf 'qemu_exit=%s\n' "$status"
grep -E 'Linux version|TILO_LANDLOCK|tilo-sandbox-exec|Kernel panic|reboot: Power down' "$out/serial.log"
test "$status" = 0
grep -q '^TILO_LANDLOCK_PRIVATE_TMP_PASS' "$out/serial.log"
