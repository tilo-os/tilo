#!/bin/bash
# Enroll Rust-generated owner keys in OVMF, then prove enforcement with signed
# and unsigned instances of the same EFI application.
set -euo pipefail

die() {
  echo "$*" >&2
  exit 1
}

sha256_file() {
  python3 - "$1" <<'PY'
import hashlib
from pathlib import Path
import sys

digest = hashlib.sha256(Path(sys.argv[1]).read_bytes()).hexdigest()
print(digest)
PY
}

resolve_aarch64_qemuvars() {
  local descriptor="$1" selection="$2"
  python3 - "$descriptor" "$selection" <<'PY'
import json
import os
from pathlib import Path
import stat
import sys

descriptor = sys.argv[1]
selection = Path(sys.argv[2])
limit = 64 * 1024


def fail(message):
    print(message, file=sys.stderr)
    raise SystemExit(1)


def validate_path(path, label):
    if not isinstance(path, str) or not path:
        fail(f"descriptor lacks {label} path")
    if any(ord(character) < 32 or ord(character) == 127 for character in path) or "," in path:
        fail(f"AArch64 Secure Boot {label} path contains unsafe characters")
    if not os.path.isabs(path) or os.path.normpath(path) != path:
        fail(f"AArch64 Secure Boot {label} path must be absolute and canonical")
    try:
        metadata = os.lstat(path)
    except FileNotFoundError:
        fail(f"missing AArch64 Secure Boot {label}: {path}")
    except OSError as error:
        fail(f"cannot inspect AArch64 Secure Boot {label}: {path}: {error}")
    if stat.S_ISLNK(metadata.st_mode):
        fail(f"symlinked AArch64 Secure Boot {label}: {path}")
    if not stat.S_ISREG(metadata.st_mode):
        fail(f"non-regular AArch64 Secure Boot {label}: {path}")
    if os.path.realpath(path) != path:
        fail(f"AArch64 Secure Boot {label} path must be absolute and canonical")
    return metadata


descriptor_metadata = validate_path(descriptor, "firmware descriptor")
if descriptor_metadata.st_size > limit:
    fail("AArch64 firmware descriptor exceeds 65536 bytes")
try:
    descriptor_bytes = Path(descriptor).read_bytes()
    document = json.loads(descriptor_bytes)
except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
    fail(f"malformed AArch64 firmware descriptor: {error}")
if not isinstance(document, dict):
    fail("AArch64 firmware descriptor root must be an object")

interfaces = document.get("interface-types")
if not isinstance(interfaces, list) or "uefi" not in interfaces:
    fail("AArch64 firmware descriptor lacks uefi interface")

targets = document.get("targets")
target_matches = False
if isinstance(targets, list):
    for target in targets:
        if not isinstance(target, dict) or target.get("architecture") != "aarch64":
            continue
        machines = target.get("machines")
        if isinstance(machines, list) and "virt-*" in machines:
            target_matches = True
            break
if not target_matches:
    fail("AArch64 firmware descriptor lacks aarch64 virt-* target")

features = document.get("features")
if not isinstance(features, list):
    features = []
if "secure-boot" not in features:
    fail("AArch64 firmware descriptor lacks secure-boot feature")
if "host-uefi-vars" not in features:
    fail("AArch64 firmware descriptor lacks host-uefi-vars feature")
if "enrolled-keys" in features:
    fail("AArch64 firmware descriptor must not use enrolled-keys")

mapping = document.get("mapping")
if not isinstance(mapping, dict) or mapping.get("device") != "memory":
    fail("AArch64 firmware mapping.device must be memory")
firmware = mapping.get("filename")
uefi_vars = mapping.get("uefi-vars")
if not isinstance(uefi_vars, dict) or not isinstance(uefi_vars.get("template"), str):
    fail("AArch64 firmware descriptor lacks mapping.uefi-vars.template")
template = uefi_vars["template"]
if firmware == template:
    fail("AArch64 firmware and variable template must differ")

validate_path(firmware, "firmware")
template_metadata = validate_path(template, "variable template")
if template_metadata.st_size > limit:
    fail("AArch64 variable template exceeds 65536 bytes")
try:
    json.loads(Path(template).read_bytes())
except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
    fail(f"malformed AArch64 variable template: {error}")

selection.write_text(f"{firmware}\n{template}\n")
PY

  exec 3< "$selection"
  IFS= read -r firmware_code <&3 \
    || die "AArch64 firmware descriptor emitted no firmware path"
  IFS= read -r firmware_vars <&3 \
    || die "AArch64 firmware descriptor emitted no variable template path"
  if IFS= read -r extra_selection <&3; then
    exec 3<&-
    die "AArch64 firmware descriptor emitted unexpected selection data"
  fi
  exec 3<&-
}

parse_stat_metadata() {
  python3 - "$1" <<'PY'
from pathlib import Path
import re
import sys

record = Path(sys.argv[1]).read_bytes()
match = re.fullmatch(rb"(0|[1-9][0-9]*):([0-7]{3}|[0-7]{4})\n", record)
if match is None:
    raise SystemExit(1)
sys.stdout.write(record[:-1].decode("ascii"))
PY
}

qualify_aarch64_firmware_inputs() {
  command -v rpm >/dev/null 2>&1 \
    || die "required rpm utility not found on PATH"
  command -v stat >/dev/null 2>&1 \
    || die "required stat utility not found on PATH"

  local path owner package="" metadata metadata_file uid mode mode_value
  metadata_file="$work/stat-metadata"
  for path in "$firmware_descriptor" "$firmware_code" "$firmware_vars"; do
    if ! owner=$(rpm -qf --qf '%{NEVRA}' -- "$path"); then
      die "AArch64 firmware input is not owned by an RPM package: $path"
    fi
    if [ -z "$owner" ] || [[ "$owner" == *$'\n'* ]] || [[ "$owner" =~ [[:space:]] ]]; then
      die "malformed RPM owner for AArch64 firmware input: $path"
    fi
    if [ -z "$package" ]; then
      package="$owner"
    elif [ "$package" != "$owner" ]; then
      die "AArch64 firmware inputs do not share one RPM owner"
    fi

    if ! stat -c '%u:%a' -- "$path" > "$metadata_file"; then
      die "stat metadata query failed for AArch64 firmware input: $path"
    fi
    if ! metadata=$(parse_stat_metadata "$metadata_file"); then
      die "malformed stat metadata for AArch64 firmware input: $path"
    fi
    uid="${metadata%%:*}"
    mode="${metadata#*:}"
    if [ "$uid" != 0 ]; then
      die "AArch64 firmware input is not root-owned: $path"
    fi
    mode_value=$((8#$mode))
    if (( (mode_value & 8#22) != 0 )); then
      die "AArch64 firmware input is group/world-writable: $path"
    fi
  done

  rpm -V "$package" >/dev/null 2>&1 \
    || die "AArch64 firmware RPM verification failed: $package"

  printf 'aarch64-firmware-descriptor=%s sha256=%s\n' \
    "$firmware_descriptor" "$(sha256_file "$firmware_descriptor")"
  printf 'aarch64-firmware-code=%s sha256=%s\n' \
    "$firmware_code" "$(sha256_file "$firmware_code")"
  printf 'aarch64-firmware-template=%s sha256=%s\n' \
    "$firmware_vars" "$(sha256_file "$firmware_vars")"
}

require_aarch64_qemuvars_capability() {
  local help
  command -v "$qemu" >/dev/null 2>&1 \
    || die "required qemu-system-aarch64 not found on PATH"
  if ! help=$("$qemu" -device uefi-vars-sysbus,help 2>&1); then
    die "qemu-system-aarch64 device help failed for uefi-vars-sysbus"
  fi
  grep -Fq 'uefi-vars-sysbus' <<< "$help" \
    || die "qemu-system-aarch64 lacks uefi-vars-sysbus"
  grep -Eq '(^|[[:space:]])jsonfile([=<[:space:]]|$)' <<< "$help" \
    || die "uefi-vars-sysbus lacks jsonfile property"
}

validate_enrolled_json_store() {
  python3 - "$1" "$2" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

store = Path(sys.argv[1])
template = Path(sys.argv[2])
try:
    json.loads(store.read_bytes())
except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
    print(f"AArch64 enrollment left an invalid JSON variable store: {error}", file=sys.stderr)
    raise SystemExit(1)
if hashlib.sha256(store.read_bytes()).digest() == hashlib.sha256(template.read_bytes()).digest():
    print("AArch64 enrollment did not change the JSON variable store", file=sys.stderr)
    raise SystemExit(1)
PY
}

verify_private_json_stores() {
  python3 - "$1" "$2" "$3" <<'PY'
import hashlib
import os
from pathlib import Path
import sys

paths = [Path(value) for value in sys.argv[1:]]
if len({str(path) for path in paths}) != 3:
    print("AArch64 VM stages do not have distinct variable-store paths", file=sys.stderr)
    raise SystemExit(1)
metadata = [os.lstat(path) for path in paths]
inodes = [(entry.st_dev, entry.st_ino) for entry in metadata]
if len(set(inodes)) != 3:
    print("AArch64 VM stages do not have distinct variable-store inodes", file=sys.stderr)
    raise SystemExit(1)
digests = [hashlib.sha256(path.read_bytes()).hexdigest() for path in paths]
if len(set(digests)) != 1:
    print("AArch64 private variable stores do not share the enrollment baseline", file=sys.stderr)
    raise SystemExit(1)
for path, entry, digest in zip(paths, metadata, digests):
    if entry.st_mode & 0o777 != 0o600:
        print(f"AArch64 private variable store is not mode 0600: {path}", file=sys.stderr)
        raise SystemExit(1)
    print(f"aarch64-private-vars={path} sha256={digest} inode={entry.st_ino}")
PY
}

set_firmware_args() {
  local variable_store="$1"
  if [ "$arch" = x86_64 ]; then
    firmware_args=(
      -drive "if=pflash,format=raw,readonly=on,file=$firmware_code"
      -drive "if=pflash,format=raw,file=$variable_store"
    )
  else
    firmware_args=(
      -bios "$firmware_code"
      -device "uefi-vars-sysbus,jsonfile=$variable_store"
    )
  fi
}

: "${TILO_KERNEL_AUDIT_DIR:=/data/tilo-kernel-audit}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
arch="${TILO_SECUREBOOT_VM_ARCH:-$(uname -m)}"
kernel="${TILO_KERNEL_PATH:-$TILO_KERNEL_AUDIT_DIR/linux/arch/x86/boot/bzImage}"
gen_cpio="${TILO_GEN_INIT_CPIO:-$TILO_KERNEL_AUDIT_DIR/linux/usr/gen_init_cpio}"
crypto="${TILO_CRYPTO_BIN:-$repo/target/release/tilo-crypto}"
case "$arch" in
  x86_64)
    firmware_code="${TILO_SECUREBOOT_VM_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.secboot.fd}"
    firmware_vars="${TILO_SECUREBOOT_VM_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}"
    qemu=qemu-system-x86_64
    efi_target=x86_64-pc-win32-coff
    ld_emulation=i386pep
    boot_name=BOOTX64.EFI
    console=ttyS0
    ;;
  aarch64)
    [ "${TILO_SECUREBOOT_VM_CODE+x}" != x ] \
      || die "TILO_SECUREBOOT_VM_CODE is unsupported on aarch64"
    [ "${TILO_SECUREBOOT_VM_VARS+x}" != x ] \
      || die "TILO_SECUREBOOT_VM_VARS is unsupported on aarch64"
    firmware_descriptor="${TILO_SECUREBOOT_VM_DESCRIPTOR:-/usr/share/qemu/firmware/91-edk2-aarch64-qemuvars-sb.json}"
    qemu=qemu-system-aarch64
    efi_target=aarch64-pc-win32-coff
    ld_emulation=aarch64pe
    boot_name=BOOTAA64.EFI
    console=ttyAMA0
    ;;
  *) die "unsupported Secure Boot VM architecture: $arch" ;;
esac

work_root="${TILO_SECUREBOOT_VM_WORK_ROOT:-$TILO_KERNEL_AUDIT_DIR}"
mkdir -p "$work_root"
work="$(mktemp -d "$work_root/secureboot-vm.XXXXXX")"
printf 'secureboot-vm-work=%s\n' "$work"

for required in "$kernel" "$gen_cpio" "$crypto"; do
  test -f "$required" || die "missing Secure Boot VM input: $required"
done
if [ "$arch" = x86_64 ]; then
  for required in "$firmware_code" "$firmware_vars"; do
    test -f "$required" || die "missing Secure Boot VM input: $required"
  done
else
  resolve_aarch64_qemuvars "$firmware_descriptor" "$work/firmware-selection"
  qualify_aarch64_firmware_inputs
  require_aarch64_qemuvars_capability
fi

if [ -n "${TILO_SECUREBOOT_VM_APP:-}" ]; then
  test -f "$TILO_SECUREBOOT_VM_APP" \
    || die "missing prebuilt EFI probe: $TILO_SECUREBOOT_VM_APP"
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
list="$work/initramfs.list"
python3 "$repo/os/tests/secureboot_initramfs.py" "$list" \
  --init "$work/init" --crypto "$crypto" --material-dir "$work"
"$gen_cpio" -o "$work/enroll.cpio" "$list"

if [ "$arch" = x86_64 ]; then
  enrollment_vars="$work/vars.enrolled.fd"
  cp "$firmware_vars" "$enrollment_vars"
  machine=(-machine q35,accel=tcg -cpu max)
else
  enrollment_vars="$work/vars.enrollment.json"
  cp "$firmware_vars" "$enrollment_vars"
  chmod 0600 "$enrollment_vars"
  machine=(-machine virt,accel=tcg,gic-version=max -cpu max)
fi

set_firmware_args "$enrollment_vars"
status=0
timeout 40s "$qemu" "${machine[@]}" -m 1024 \
  "${firmware_args[@]}" \
  -kernel "$kernel" -initrd "$work/enroll.cpio" \
  -append "console=$console rdinit=/init panic=-1" -nographic -no-reboot \
  > "$work/enroll.log" 2>&1 || status=$?
if [ "$arch" = aarch64 ]; then
  [ "$status" -eq 0 ] || { cat "$work/enroll.log"; die "AArch64 enrollment VM did not exit cleanly: $status"; }
else
  case "$status" in 0|124) ;; *) cat "$work/enroll.log"; exit "$status" ;; esac
fi
grep -F 'TILO_SECURE_BOOT_ENROLL_PASS' "$work/enroll.log"
grep -F 'TILO_SECURE_BOOT_SETUP_MODE=1' "$work/enroll.log"
grep -F 'TILO_SECURE_BOOT_SECURE_BOOT=0' "$work/enroll.log"

make_esp() {
  local image="$1" executable="$2"
  truncate -s 64M "$image"
  mkfs.vfat -n TILO_SB "$image" >/dev/null
  mmd -i "$image" ::/EFI ::/EFI/BOOT
  mcopy -i "$image" "$executable" "::/EFI/BOOT/$boot_name"
  mcopy -i "$image" "$repo/os/tests/fixtures/secureboot-startup.nsh" ::/startup.nsh
}
make_esp "$work/signed.img" "$work/app.signed.efi"
make_esp "$work/unsigned.img" "$work/app.efi"

if [ "$arch" = x86_64 ]; then
  signed_vars="$work/vars.signed.fd"
  unsigned_vars="$work/vars.unsigned.fd"
  cp "$enrollment_vars" "$signed_vars"
  cp "$enrollment_vars" "$unsigned_vars"
else
  validate_enrolled_json_store "$enrollment_vars" "$firmware_vars"
  unsigned_vars="$work/vars.unsigned.json"
  signed_vars="$work/vars.signed.json"
  cp "$enrollment_vars" "$unsigned_vars"
  cp "$enrollment_vars" "$signed_vars"
  chmod 0600 "$unsigned_vars" "$signed_vars"
  verify_private_json_stores "$enrollment_vars" "$unsigned_vars" "$signed_vars"
fi

boot_efi() {
  local vars="$1" disk="$2" log="$3"
  set_firmware_args "$vars"
  timeout 35s "$qemu" "${machine[@]}" -m 512 \
    "${firmware_args[@]}" \
    -drive "if=virtio,format=raw,file=$disk" -nographic -no-reboot \
    > "$log" 2>&1 || test "$?" -eq 124
}
boot_efi "$unsigned_vars" "$work/unsigned.img" "$work/unsigned.log"
grep -F 'Access Denied -- rejected probably by Secure Boot' "$work/unsigned.log"
if grep -Fq 'TILO_SECURE_BOOT_SIGNED_EFI_PASS' "$work/unsigned.log"; then
  die 'unsigned EFI application executed under enrolled Secure Boot'
fi
boot_efi "$signed_vars" "$work/signed.img" "$work/signed.log"
grep -F 'TILO_SECURE_BOOT_SIGNED_EFI_PASS' "$work/signed.log"
printf 'secureboot-vm-probe=PASS\n'
