#!/bin/bash
# Enroll Rust-generated owner keys in OVMF, then prove enforcement with signed
# and unsigned instances of the same EFI application.
set -euo pipefail

: "${TILO_KERNEL_AUDIT_DIR:=/data/tilo-kernel-audit}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
arch="${TILO_SECUREBOOT_VM_ARCH:-$(uname -m)}"
kernel="${TILO_KERNEL_PATH:-$TILO_KERNEL_AUDIT_DIR/linux/arch/x86/boot/bzImage}"
gen_cpio="${TILO_GEN_INIT_CPIO:-$TILO_KERNEL_AUDIT_DIR/linux/usr/gen_init_cpio}"
crypto="${TILO_CRYPTO_BIN:-$repo/target/release/tilo-crypto}"
case "$arch" in
  x86_64)
    for directory in /usr/share/edk2/ovmf /usr/share/OVMF; do
      if [ -f "$directory/OVMF_CODE.secboot.fd" ] \
         && [ -f "$directory/OVMF_VARS.fd" ]; then
        firmware_code="$directory/OVMF_CODE.secboot.fd"
        firmware_vars="$directory/OVMF_VARS.fd"
        break
      elif [ -f "$directory/OVMF_CODE_4M.secboot.fd" ] \
           && [ -f "$directory/OVMF_VARS_4M.fd" ]; then
        firmware_code="$directory/OVMF_CODE_4M.secboot.fd"
        firmware_vars="$directory/OVMF_VARS_4M.fd"
        break
      fi
    done
    firmware_code="${TILO_SECUREBOOT_VM_CODE:-${firmware_code:-}}"
    firmware_vars="${TILO_SECUREBOOT_VM_VARS:-${firmware_vars:-}}"
    qemu=qemu-system-x86_64
    efi_target=x86_64-pc-win32-coff
    ld_emulation=i386pep
    boot_name=BOOTX64.EFI
    console=ttyS0
    ;;
  aarch64)
    for directory in /usr/share/edk2/aarch64 /usr/share/AAVMF /usr/share/qemu-efi-aarch64; do
      if [ -f "$directory/AAVMF_CODE.ms.fd" ] \
         && [ -f "$directory/AAVMF_VARS.fd" ]; then
        # Ubuntu's generic AAVMF_CODE.fd omits authenticated Secure Boot
        # variables. Use its Secure-Boot-capable code with the blank variable
        # template so this probe starts in setup mode and enrolls Tilo's keys.
        firmware_code="$directory/AAVMF_CODE.ms.fd"
        firmware_vars="$directory/AAVMF_VARS.fd"
        break
      elif [ -f "$directory/QEMU_EFI-qemuvars-pflash.raw" ] \
         && [ -f "$directory/vars.blank.json" ]; then
        # Fedora's Secure-Boot-capable aarch64 firmware keeps its variables in QEMU's
        # uefi-vars device rather than in a flash image; its plain QEMU_EFI-pflash.raw has
        # no Secure Boot. The blank store starts in setup mode.
        firmware_code="$directory/QEMU_EFI-qemuvars-pflash.raw"
        firmware_vars="$directory/vars.blank.json"
        break
      elif [ -f "$directory/QEMU_EFI-pflash.raw" ]; then
        firmware_code="$directory/QEMU_EFI-pflash.raw"
        firmware_vars="$directory/vars-template-pflash.raw"
        break
      elif [ -f "$directory/AAVMF_CODE.fd" ]; then
        firmware_code="$directory/AAVMF_CODE.fd"
        firmware_vars="$directory/AAVMF_VARS.fd"
        break
      fi
    done
    firmware_code="${TILO_SECUREBOOT_VM_CODE:-${firmware_code:-}}"
    firmware_vars="${TILO_SECUREBOOT_VM_VARS:-${firmware_vars:-}}"
    qemu=qemu-system-aarch64
    efi_target=aarch64-pc-win32-coff
    ld_emulation=aarch64pe
    boot_name=BOOTAA64.EFI
    console=ttyAMA0
    ;;
  *) echo "unsupported Secure Boot VM architecture: $arch" >&2; exit 1 ;;
esac
work_root="${TILO_SECUREBOOT_VM_WORK_ROOT:-$TILO_KERNEL_AUDIT_DIR}"
mkdir -p "$work_root"
work="$(mktemp -d "$work_root/secureboot-vm.XXXXXX")"
printf 'secureboot-vm-work=%s\n' "$work"

expect_log() {
  local marker="$1" log="$2"
  if ! grep -F "$marker" "$log"; then
    printf 'missing expected marker %s in %s\n' "$marker" "$log" >&2
    cat "$log" >&2
    return 1
  fi
}

for required in "$kernel" "$gen_cpio" "$crypto" "$firmware_code" "$firmware_vars"; do
  test -f "$required" || { echo "missing Secure Boot VM input: $required" >&2; exit 1; }
done

if [ -n "${TILO_SECUREBOOT_VM_APP:-}" ]; then
  test -f "$TILO_SECUREBOOT_VM_APP" \
    || { echo "missing prebuilt EFI probe: $TILO_SECUREBOOT_VM_APP" >&2; exit 1; }
  cp "$TILO_SECUREBOOT_VM_APP" "$work/app.efi"
else
  clang -target "$efi_target" -ffreestanding -fno-stack-protector \
    -fshort-wchar -c "$repo/os/tests/secureboot_vm_app.c" -o "$work/app.obj"
  ld -m"$ld_emulation" --subsystem 10 -e efi_main "$work/app.obj" -o "$work/app.efi"
fi

for key in PK KEK db; do
  "$crypto" generate-secure-boot-cert "Tilo VM $key" \
    "$work/$key.key" "$work/$key.crt"
  "$crypto" certificate-esl "$work/$key.crt" "$work/$key.esl"
done
"$crypto" sign-efi "$work/db.key" "$work/db.crt" \
  "$work/app.efi" "$work/app.signed.efi"
"$crypto" verify-efi "$work/db.crt" "$work/app.signed.efi"

gcc -O2 -Wall -Wextra -Werror -o "$work/init" \
  "$repo/os/tests/secureboot_vm_enroll.c"
ldd "$work/init" > "$work/init.ldd"
ldd "$crypto" > "$work/crypto.ldd"
interpreter=$(readelf -l "$work/init" \
  | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
test -f "$interpreter"
mapfile -t runtime_files < <(
  {
    printf '%s\n' "$interpreter"
    awk '/=> \// { print $3 } $1 ~ /^\// { print $1 }' \
      "$work/init.ldd" "$work/crypto.ldd"
  } | sort -u
)
runtime_dirs=()
for dependency in "${runtime_files[@]}"; do
  test -f "$dependency"
  directory=$(dirname "$dependency")
  while [ "$directory" != / ]; do
    runtime_dirs+=("$directory")
    directory=$(dirname "$directory")
  done
done
mapfile -t runtime_dirs < <(printf '%s\n' "${runtime_dirs[@]}" | sort -u)
list="$work/initramfs.list"
{
  printf 'dir /sys 0755 0 0\n'
  for directory in "${runtime_dirs[@]}"; do
    printf 'dir %s 0755 0 0\n' "$directory"
  done
  printf 'file /init %s 0755 0 0\n' "$work/init"
  printf 'file /tilo-crypto %s 0755 0 0\n' "$crypto"
  for key in PK KEK db; do
    printf 'file /%s.esl %s 0600 0 0\n' "$key" "$work/$key.esl"
  done
  printf 'file /PK.key %s 0600 0 0\n' "$work/PK.key"
  printf 'file /PK.crt %s 0600 0 0\n' "$work/PK.crt"
  for dependency in "${runtime_files[@]}"; do
    printf 'file %s %s 0755 0 0\n' "$dependency" "$dependency"
  done
} > "$list"
"$gen_cpio" -o "$work/enroll.cpio" "$list"

cp "$firmware_vars" "$work/vars.enrolled.fd"
# The QEMU arguments for variable store $1 in vars_args: a JSON store is QEMU's own
# (uefi-vars), not a second flash drive.
vars_args() {
  case "$firmware_vars" in
    *.json) vars_args=(-device "uefi-vars-sysbus,jsonfile=$1") ;;
    *) vars_args=(-drive "if=pflash,format=raw,file=$1") ;;
  esac
}
vars_args "$work/vars.enrolled.fd"
if [ "$arch" = x86_64 ]; then
  machine=(-machine q35,accel=tcg -cpu max)
else
  machine=(-machine virt,accel=tcg,gic-version=max -cpu max)
fi
timeout 40s "$qemu" "${machine[@]}" -m 1024 \
  -drive if=pflash,format=raw,readonly=on,file="$firmware_code" "${vars_args[@]}" \
  -kernel "$kernel" -initrd "$work/enroll.cpio" \
  -append "console=$console rdinit=/init panic=-1" -nic none -nographic -no-reboot \
  > "$work/enroll.log" 2>&1 || status=$?
status=${status:-0}
case "$status" in 0|124) ;; *) cat "$work/enroll.log"; exit "$status" ;; esac
expect_log 'TILO_SECURE_BOOT_ENROLL_PASS' "$work/enroll.log"
expect_log 'TILO_SECURE_BOOT_SETUP_MODE=1' "$work/enroll.log"
expect_log 'TILO_SECURE_BOOT_SECURE_BOOT=0' "$work/enroll.log"

make_esp() {
  local image="$1" executable="$2"
  truncate -s 64M "$image"
  mkfs.vfat -n TILO_SBTEST "$image" >/dev/null
  mmd -i "$image" ::/EFI ::/EFI/BOOT
  mcopy -i "$image" "$executable" "::/EFI/BOOT/$boot_name"
  mcopy -i "$image" "$repo/os/tests/fixtures/secureboot-startup.nsh" ::/startup.nsh
}
make_esp "$work/signed.img" "$work/app.signed.efi"
make_esp "$work/unsigned.img" "$work/app.efi"
cp "$work/vars.enrolled.fd" "$work/vars.signed.fd"
cp "$work/vars.enrolled.fd" "$work/vars.unsigned.fd"

boot_efi() {
  local vars="$1" disk="$2" log="$3"
  local -a disk_args
  if [ "$arch" = aarch64 ]; then
    # AAVMF includes the virtio-mmio block driver; avoid requiring the
    # distribution-specific efi-virtio.rom PCI option ROM on ARM runners.
    disk_args=(-drive "if=none,id=esp,format=raw,file=$disk"
               -device virtio-blk-device,drive=esp)
  else
    disk_args=(-drive "if=virtio,format=raw,file=$disk")
  fi
  vars_args "$vars"
  timeout 35s "$qemu" "${machine[@]}" -m 512 \
    -drive if=pflash,format=raw,readonly=on,file="$firmware_code" "${vars_args[@]}" \
    "${disk_args[@]}" -nic none -nographic -no-reboot \
    > "$log" 2>&1 || test "$?" -eq 124
}
boot_efi "$work/vars.unsigned.fd" "$work/unsigned.img" "$work/unsigned.log"
expect_log 'Access Denied' "$work/unsigned.log"
if grep -Fq 'TILO_SECURE_BOOT_SIGNED_EFI_PASS' "$work/unsigned.log"; then
  echo 'unsigned EFI application executed under enrolled Secure Boot' >&2
  exit 1
fi
boot_efi "$work/vars.signed.fd" "$work/signed.img" "$work/signed.log"
expect_log 'TILO_SECURE_BOOT_SIGNED_EFI_PASS' "$work/signed.log"
printf 'secureboot-vm-probe=PASS\n'
