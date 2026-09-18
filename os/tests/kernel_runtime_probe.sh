#!/bin/bash
# Boot a compiled Tilo kernel and exercise its enforced module trust boundary.
set -euo pipefail

: "${TILO_KERNEL_AUDIT_DIR:=/data/tilo-kernel-audit}"
kernel="$TILO_KERNEL_AUDIT_DIR/linux/arch/x86/boot/bzImage"
gen_cpio="$TILO_KERNEL_AUDIT_DIR/linux/usr/gen_init_cpio"
module="$TILO_KERNEL_AUDIT_DIR/stage/lib/modules/6.19.14/kernel/drivers/net/dummy.ko.zst"
work="$TILO_KERNEL_AUDIT_DIR/runtime-probe"
mkdir -p "$work"

for required in "$kernel" "$gen_cpio" "$module"; do
  test -f "$required" || { echo "missing runtime-probe input: $required" >&2; exit 1; }
done

gcc -O2 -Wall -Wextra -Werror -o "$work/init" \
  "$(dirname "$0")/kernel_runtime_probe.c"
zstd -q -d -f "$module" -o "$work/signed.ko"

loader=$(ldd "$work/init" | awk '/ld-linux/ { print $1; exit }')
libc=$(ldd "$work/init" | awk '/libc\.so/ { print $3; exit }')
test -f "$loader"
test -f "$libc"

list="$work/initramfs.list"
{
  printf 'dir /dev 0755 0 0\n'
  printf 'nod /dev/console 0600 0 0 c 5 1\n'
  printf 'dir /lib64 0755 0 0\n'
  printf 'file /init %s 0755 0 0\n' "$work/init"
  printf 'file /signed.ko %s 0600 0 0\n' "$work/signed.ko"
  printf 'file %s %s 0755 0 0\n' "$loader" "$loader"
  printf 'file %s %s 0755 0 0\n' "$libc" "$libc"
} > "$list"
"$gen_cpio" -o "$work/initramfs.cpio" "$list"

set +e
timeout 30s qemu-system-x86_64 \
  -machine q35,accel=tcg -cpu max -m 1024 -smp 2 \
  -kernel "$kernel" -initrd "$work/initramfs.cpio" \
  -append 'console=ttyS0 rdinit=/init module.sig_enforce=1 panic=-1' \
  -nographic -no-reboot > "$work/console.log" 2>&1
qemu_status=$?
set -e
case "$qemu_status" in 0|124) ;; *) cat "$work/console.log"; exit "$qemu_status" ;; esac

grep -F 'TILO_TAMPERED_MODULE rc=-1 errno=129' "$work/console.log"
grep -F 'TILO_SIGNED_MODULE rc=0 errno=0' "$work/console.log"
grep -F 'TILO_GAME_SCHEDULING uclamp_min=256 timer_slack_ns=1000 ioprio=be/2' \
  "$work/console.log"
grep -F 'PREEMPT(full)' "$work/console.log"
grep -F 'TILO_RUNTIME_PROBE_PASS' "$work/console.log"
echo 'kernel-runtime-probe=PASS'
