#!/usr/bin/env bash
# Release-grade live-medium -> encrypted target -> first-login cycle.
#
# The live installer is driven through the actions it publishes — the same
# addresses its own buttons invoke — over its console channel: a virtio-serial
# port that only this VM's command line attaches (os/dev/vm.sh console;
# crates/session/console-control), speaking the tilo-control action protocol.
# Its state, never including the recovery key, is the completion oracle. The
# installed target has no such channel yet, so its unlock prompt, first-login
# OOBE and desktop are driven with QEMU keyboard events and observed from the
# real framebuffer with OCR. No SSH service, shot mode, backend invocation, or
# target-disk modification is used. The default no-TPM path proves that the
# passphrase enrolled by the installer unlocks early userspace after a cold
# power cycle. The opt-in software-TPM path additionally requires an enforcing
# variable store that trusts the released UKI; it fails rather than calling
# setup-mode boot a verified-boot test.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
vm="$dev_dir/vm.sh"
medium="${1:?usage: test-installer-cycle.sh INSTALLER_QCOW2 [--out DIRECTORY] [--prepared-alongside|--guided-alongside] [--install-only] [--software-tpm] [--graphics gl|software] [--build-profile-encoder] [--selinux-audit-ssh]}"
shift
out=""
install_mode=erase
software_tpm=0
graphics=gl
build_profile_encoder=0
install_only=0
selinux_audit_ssh=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:-}"; shift 2;;
    --prepared-alongside) install_mode=prepared-alongside; shift;;
    --guided-alongside) install_mode=guided-alongside; shift;;
    --install-only) install_only=1; shift;;
    --software-tpm) software_tpm=1; shift;;
    --build-profile-encoder) build_profile_encoder=1; shift;;
    --selinux-audit-ssh) selinux_audit_ssh=1; shift;;
    --graphics) graphics="${2:-}"; [ "$#" -ge 2 ] || exit 2; shift 2;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2;;
  esac
done
expected_target_ref="${TILO_EXPECTED_INSTALLED_REF:-}"
if [ "$install_only" -eq 1 ]; then
  [ "$install_mode" = guided-alongside ] \
    || { echo '--install-only requires --guided-alongside' >&2; exit 2; }
fi
if [ -n "$expected_target_ref" ] || [ "$install_only" -eq 1 ]; then
  [ "$install_mode" = guided-alongside ] \
    || { echo 'signed offline target proof requires --guided-alongside' >&2; exit 2; }
  [[ "$expected_target_ref" =~ ^registry\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] \
    || { echo 'TILO_EXPECTED_INSTALLED_REF must pin the separately signed target' >&2; exit 2; }
fi
case "$graphics" in
  gl) display_arg=--gl ;;
  software) display_arg=--2d ;;
  *) printf 'graphics must be gl or software\n' >&2; exit 2 ;;
esac
[ -f "$medium" ] || { printf 'installer medium not found: %s\n' "$medium" >&2; exit 1; }
if [ "$software_tpm" -eq 1 ]; then
  case "${TILO_SECBOOT_KEYS:-none}" in
    none|persist|microsoft)
      printf '%s\n' 'software-TPM cycle requires TILO_SECBOOT_KEYS=/path/to/pre-enrolled-vars with the Tilo release certificate in db; setup mode and the Microsoft-only store do not trust the owner-signed UKI' >&2
      exit 2
      ;;
  esac
fi
# Remote acceptance hosts must use the pinned standalone production encoder.
# Compiling it is an explicit local-development option, never an implicit GUI
# workspace build during a VM acceptance run.
encoder="${TILO_PROFILE_ENCODER:-}"
encoder_sha="${TILO_PROFILE_ENCODER_SHA256:-}"
if [ "$build_profile_encoder" -eq 1 ]; then
  [ -z "$encoder$encoder_sha" ] || { echo 'choose prebuilt encoder or --build-profile-encoder' >&2; exit 2; }
  encoder_command=(cargo run --quiet --release --locked --manifest-path "$dev_dir/../../tools/imager-profile/Cargo.toml" --)
  encoder_sha=explicit-local-build
else
  [[ "$encoder" = /* ]] && [ -f "$encoder" ] && [ ! -L "$encoder" ] && [ -x "$encoder" ] \
    || { echo 'TILO_PROFILE_ENCODER must name an absolute regular executable' >&2; exit 2; }
  [[ "$encoder_sha" =~ ^[0-9a-f]{64}$ ]] \
    || { echo 'TILO_PROFILE_ENCODER_SHA256 must pin the prebuilt encoder' >&2; exit 2; }
  [ "$(sha256sum "$encoder" | awk '{print $1}')" = "$encoder_sha" ] \
    || { echo 'profile encoder SHA256 differs' >&2; exit 2; }
  encoder_command=("$encoder")
fi

command -v tesseract >/dev/null 2>&1 \
  || { printf 'tesseract is required for framebuffer verification\n' >&2; exit 1; }
if [ "$selinux_audit_ssh" -eq 1 ]; then
  # Guest audit inspection lives in installer-selinux-audit.sh so this release
  # cycle script never grows a guest-shell inspection channel.
  "$dev_dir/installer-selinux-audit.sh" require-host \
    || { echo '--selinux-audit-ssh host prerequisites failed' >&2; exit 2; }
fi

out="${out:-$PWD/installer-cycle-artifacts}"
export TILO_VM_CYCLE="${TILO_VM_CYCLE:-installer-cycle-$$}"
storage="${TILO_INSTALLER_CYCLE_STORAGE:-$(dirname "$out")/.$(basename "$out")-storage-$TILO_VM_CYCLE}"
run="${TILO_RUN:-${XDG_RUNTIME_DIR:-/tmp}/tilo-vm-$TILO_VM_CYCLE}"
cycle_dir="$storage/vm-cycles/$TILO_VM_CYCLE"
mkdir -p "$out" "$storage"

export TILO_INSTALLER_OUTPUT="$storage"
export TILO_RUN="$run"
export TILO_INSTALLER_DISK="$medium"
export TILO_INSTALLER_WORK_DISK="$cycle_dir/installer-work.qcow2"
export TILO_TARGET_DISK="$cycle_dir/target.qcow2"
export TILO_TARGET_SIZE="${TILO_TARGET_SIZE:-40G}"
export TILO_USER="${TILO_INSTALLER_CYCLE_USER:-cycleuser}"
if [ "$software_tpm" -eq 1 ]; then
  export TILO_PASS="${TILO_INSTALLER_CYCLE_PIN:-804927}"
  unlock=software-tpm-pin
else
  export TILO_PASS="${TILO_INSTALLER_CYCLE_PASSPHRASE:-cycle-passphrase-2026}"
  unlock=no-tpm-passphrase
fi
export TILO_BOOT_WAIT_SECS="${TILO_BOOT_WAIT_SECS:-1800}"

phase=installer
browser_fixture_server=""
copy_evidence() {
  local label="$1"
  # The install backend's private stdout includes the one-time recovery key.
  # Never make a raw live-installer serial stream an uploadable artifact, even
  # if a future service configuration begins mirroring that stdout to console.
  if [ "$label" = installer ]; then
    printf 'withheld: live-installer serial may contain the one-time recovery key\n' \
      > "$out/installer-serial-withheld.txt"
  elif [ -f "$run/serial.log" ]; then
    cp -f "$run/serial.log" "$out/$label-serial.log"
  fi
  [ ! -f "$run/qemu.log" ] || cp -f "$run/qemu.log" "$out/$label-qemu.log"
  "$vm" status > "$out/$label-vm-status.txt" 2>&1 || true
}
scrub_artifact_secrets() {
  local file leaked=0
  while IFS= read -r -d '' file; do
    if LC_ALL=C grep -aEqi \
      '::tilo-recovery-key::|RECOVERY KEY.*(write|save|off-device)|[bcdfghjkmnpqrstvwxyz]{8}(-[bcdfghjkmnpqrstvwxyz]{8}){7}' \
      "$file"; then
      : > "$file"
      leaked=1
    fi
  done < <(find "$out" -type f ! -name artifact-secret-redaction.txt -print0)
  if [ "$leaked" -ne 0 ]; then
    printf 'A diagnostic containing recovery material was withheld.\n' \
      > "$out/artifact-secret-redaction.txt"
    return 1
  fi
}
# Prove the upload boundary itself rejects both the backend marker and the
# recovery-key shape before any VM work begins. The canary and redaction notice
# are removed after the expected rejection; a broken scrubber aborts the gate.
printf '%s\n%s\n' '::tilo-recovery-key::artifact-canary' \
  'bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk' \
  > "$out/.artifact-secret-canary.txt"
if scrub_artifact_secrets; then
  printf 'installer cycle: artifact secret scrubber accepted its canary\n' >&2
  exit 1
fi
test ! -s "$out/.artifact-secret-canary.txt" || exit 1
rm -f -- "$out/.artifact-secret-canary.txt" "$out/artifact-secret-redaction.txt"

cleanup() {
  local rc=$?
  trap - EXIT
  if [ -n "$browser_fixture_server" ]; then
    kill "$browser_fixture_server" >/dev/null 2>&1 || true
    wait "$browser_fixture_server" 2>/dev/null || true
  fi
  copy_evidence "$phase" || true
  "$vm" stop >/dev/null 2>&1 || true
  if command -v qemu-img >/dev/null 2>&1 && [ -f "$TILO_TARGET_DISK" ]; then
    qemu-img info "$TILO_TARGET_DISK" > "$out/target-qemu-img-info.txt" 2>&1 || true
  fi
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  scrub_artifact_secrets || rc=1
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  exit "$rc"
}
trap cleanup EXIT

prepare_alongside_target() {
  local raw="$cycle_dir/prepared-target.raw" esp="$cycle_dir/esp.img"
  local windows="$cycle_dir/windows.img" linux="$cycle_dir/linux.img"
  local fixture_builder="$dev_dir/build-foreign-boot-fixture.sh"
  local linux_loader esp_size windows_offset linux_offset
  for tool in qemu-img sfdisk mkfs.vfat mkfs.ntfs mkfs.ext4 mattrib mcopy mdir mmd mtype ntfscp debugfs; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required for alongside cycle"
  done
  # Both fixtures begin with a foreign shared ESP plus valid Windows and Linux
  # filesystems. Prepared mode supplies an empty root and a reusable 2 GiB ESP;
  # guided mode leaves the remaining >=35 GiB genuinely unallocated so the
  # installer must append its own dedicated ESP, boot and encrypted root.
  truncate -s 40G "$raw"
  if [ "$install_mode" = prepared-alongside ]; then
    esp_size=2G windows_offset=2049 linux_offset=2561
    sfdisk "$raw" >/dev/null <<'EOF'
label: gpt
start=2048, size=4194304, type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b, uuid=10000000-0000-4000-8000-000000000001, name="shared EFI"
start=4196352, size=1048576, type=ebd0a0a2-b9e5-4433-87c0-68b6b72699c7, uuid=20000000-0000-4000-8000-000000000002, name="Windows sentinel"
start=5244928, size=1048576, type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=30000000-0000-4000-8000-000000000003, name="Linux sentinel"
start=6293504, size=2097152, type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=50000000-0000-4000-8000-000000000005, name="Tilo prepared boot"
start=8390656, size=75492848, type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=40000000-0000-4000-8000-000000000004, name="Tilo prepared root"
EOF
  else
    esp_size=512M windows_offset=513 linux_offset=1025
    sfdisk "$raw" >/dev/null <<'EOF'
label: gpt
start=2048, size=1048576, type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b, uuid=10000000-0000-4000-8000-000000000001, name="shared EFI"
start=1050624, size=1048576, type=ebd0a0a2-b9e5-4433-87c0-68b6b72699c7, uuid=20000000-0000-4000-8000-000000000002, name="Windows sentinel"
start=2099200, size=1048576, type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=30000000-0000-4000-8000-000000000003, name="Linux sentinel"
EOF
  fi
  printf '%s\n' "$windows_offset" > "$cycle_dir/windows.offset-mib"
  printf '%s\n' "$linux_offset" > "$cycle_dir/linux.offset-mib"
  truncate -s "$esp_size" "$esp"; mkfs.vfat -n SHARED-EFI "$esp" >/dev/null
  mmd -i "$esp" ::/EFI ::/EFI/Microsoft ::/EFI/Microsoft/Boot ::/EFI/Fedora
  case "$TILO_VM_ARCH" in x86_64) linux_loader=shimx64.efi;; aarch64) linux_loader=shimaa64.efi;; esac
  "$fixture_builder" "$TILO_VM_ARCH" windows "$cycle_dir/bootmgfw.efi"
  "$fixture_builder" "$TILO_VM_ARCH" linux "$cycle_dir/$linux_loader"
  mcopy -i "$esp" "$cycle_dir/bootmgfw.efi" ::/EFI/Microsoft/Boot/bootmgfw.efi
  mcopy -i "$esp" "$cycle_dir/$linux_loader" "::/EFI/Fedora/$linux_loader"
  sha256sum "$cycle_dir/bootmgfw.efi" | awk '{print $1}' > "$cycle_dir/windows-loader.before.sha256"
  sha256sum "$cycle_dir/$linux_loader" | awk '{print $1}' > "$cycle_dir/linux-loader.before.sha256"
  sha256sum "$esp" | awk '{print $1}' > "$cycle_dir/foreign-esp.before.sha256"
  truncate -s 512M "$windows"; mkfs.ntfs -F -L WINDOWS-SENTINEL "$windows" >/dev/null
  truncate -s 512M "$linux"; mkfs.ext4 -q -F -L LINUX-SENTINEL "$linux"
  printf 'windows-filesystem-preservation-sentinel\n' > "$cycle_dir/windows-sentinel.txt"
  printf 'linux-filesystem-preservation-sentinel\n' > "$cycle_dir/linux-sentinel.txt"
  ntfscp "$windows" "$cycle_dir/windows-sentinel.txt" /preservation.txt
  debugfs -w -R "write $cycle_dir/linux-sentinel.txt /preservation.txt" "$linux" >/dev/null
  sha256sum "$windows" | awk '{print $1}' > "$cycle_dir/windows.before.sha256"
  sha256sum "$linux" | awk '{print $1}' > "$cycle_dir/linux.before.sha256"
  dd if="$esp" of="$raw" bs=1M seek=1 conv=notrunc,sparse status=none
  dd if="$windows" of="$raw" bs=1M seek="$windows_offset" conv=notrunc,sparse status=none
  dd if="$linux" of="$raw" bs=1M seek="$linux_offset" conv=notrunc,sparse status=none
  qemu-img convert -f raw -O qcow2 "$raw" "$TILO_TARGET_DISK"
  rm -f -- "$raw" "$esp" "$windows" "$linux" "$cycle_dir/bootmgfw.efi" \
    "$cycle_dir/$linux_loader" "$cycle_dir/windows-sentinel.txt" "$cycle_dir/linux-sentinel.txt"
}

verify_alongside_target() {
  local raw="$cycle_dir/installed-alongside.raw" actual linux_loader systemd_loader
  local windows_offset linux_offset
  qemu-img convert -f qcow2 -O raw "$TILO_TARGET_DISK" "$raw"
  windows_offset="$(cat "$cycle_dir/windows.offset-mib")"
  linux_offset="$(cat "$cycle_dir/linux.offset-mib")"
  actual="$(dd if="$raw" bs=1M skip="$windows_offset" count=512 status=none | sha256sum | awk '{print $1}')"
  [ "$actual" = "$(cat "$cycle_dir/windows.before.sha256")" ] \
    || die "Windows filesystem bytes changed during alongside install"
  actual="$(dd if="$raw" bs=1M skip="$linux_offset" count=512 status=none | sha256sum | awk '{print $1}')"
  [ "$actual" = "$(cat "$cycle_dir/linux.before.sha256")" ] \
    || die "Linux filesystem bytes changed during alongside install"
  case "$TILO_VM_ARCH" in
    x86_64) linux_loader=shimx64.efi; systemd_loader=systemd-bootx64.efi;;
    aarch64) linux_loader=shimaa64.efi; systemd_loader=systemd-bootaa64.efi;;
  esac
  actual="$(MTOOLS_SKIP_CHECK=1 mtype -i "$raw@@1048576" ::/EFI/Microsoft/Boot/bootmgfw.efi | sha256sum | awk '{print $1}')"
  [ "$actual" = "$(cat "$cycle_dir/windows-loader.before.sha256")" ] \
    || die "Windows EFI loader bytes changed during alongside install"
  actual="$(MTOOLS_SKIP_CHECK=1 mtype -i "$raw@@1048576" "::/EFI/Fedora/$linux_loader" | sha256sum | awk '{print $1}')"
  [ "$actual" = "$(cat "$cycle_dir/linux-loader.before.sha256")" ] \
    || die "Linux EFI loader bytes changed during alongside install"

  if [ "$install_mode" = prepared-alongside ]; then
    python3 - "$raw" <<'PY' || die "prepared alongside GPT layout changed"
import json, subprocess, sys

table = json.loads(subprocess.check_output(["sfdisk", "--json", sys.argv[1]]))["partitiontable"]
sector = int(table["sectorsize"])
parts = table["partitions"]
expected = [
    ("10000000-0000-4000-8000-000000000001", 1 << 20, 2 << 30),
    ("20000000-0000-4000-8000-000000000002", 2049 << 20, 512 << 20),
    ("30000000-0000-4000-8000-000000000003", 2561 << 20, 512 << 20),
    ("50000000-0000-4000-8000-000000000005", 3073 << 20, 1 << 30),
    ("40000000-0000-4000-8000-000000000004", 4097 << 20, 75492848 * 512),
]
assert len(parts) == len(expected)
for part, (uuid, start, size) in zip(parts, expected):
    assert part["uuid"].lower() == uuid and part["start"] * sector == start and part["size"] * sector == size
PY
    MTOOLS_SKIP_CHECK=1 mtype -i "$raw@@1048576" ::/loader/entries/tilo-windows.conf \
      | grep -Fqx 'efi /EFI/Microsoft/Boot/bootmgfw.efi' \
      || die "Windows chainloader entry has the wrong EFI target"
    MTOOLS_SKIP_CHECK=1 mtype -i "$raw@@1048576" ::/loader/entries/tilo-linux-fedora.conf \
      | grep -Fqx "efi /EFI/Fedora/$linux_loader" \
      || die "Linux chainloader entry has the wrong EFI target"
    printf 'boot_entries=windows,linux-fedora\n' > "$out/alongside-boot-behavior.txt"
  else
    actual="$(dd if="$raw" bs=1M skip=1 count=512 status=none | sha256sum | awk '{print $1}')"
    [ "$actual" = "$(cat "$cycle_dir/foreign-esp.before.sha256")" ] \
      || die "foreign shared EFI partition changed during guided alongside install"
    python3 - "$raw" <<'PY' || die "guided alongside GPT layout differs from the reviewed plan"
import json, subprocess, sys

disk = sys.argv[1]
table = json.loads(subprocess.check_output(["sfdisk", "--json", disk]))["partitiontable"]
sector = int(table["sectorsize"])
parts = table["partitions"]
assert len(parts) == 6, f"expected six GPT partitions, found {len(parts)}"
expected = [
    ("10000000-0000-4000-8000-000000000001", 1 << 20, 512 << 20),
    ("20000000-0000-4000-8000-000000000002", 513 << 20, 512 << 20),
    ("30000000-0000-4000-8000-000000000003", 1025 << 20, 512 << 20),
]
for part, (uuid, start, size) in zip(parts[:3], expected):
    assert part["uuid"].lower() == uuid and part["start"] * sector == start and part["size"] * sector == size
efi, boot, root = parts[3:]
assert efi["start"] * sector == 1537 << 20 and efi["size"] * sector == 2 << 30
assert efi["type"].lower() == "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
assert boot["start"] * sector == 3585 << 20 and boot["size"] * sector == 1 << 30
assert boot["type"].lower() == "0fc63daf-8483-4772-8e79-3d69d8477de4"
assert root["start"] * sector == 4609 << 20 and root["size"] * sector == 36350 << 20
assert root["type"].lower() == "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab"
assert len({efi["uuid"].lower(), boot["uuid"].lower(), root["uuid"].lower()}) == 3
assert all(p["uuid"].lower() not in {old[0] for old in expected} for p in (efi, boot, root))
PY
    MTOOLS_SKIP_CHECK=1 mattrib -i "$raw@@$((1537 * 1024 * 1024))" ::/loader/loader.conf >/dev/null \
      || die "guided Tilo EFI partition has no systemd-boot loader configuration"
    MTOOLS_SKIP_CHECK=1 mattrib -i "$raw@@$((1537 * 1024 * 1024))" "::/EFI/systemd/$systemd_loader" >/dev/null \
      || die "guided Tilo EFI partition has no architecture-matched systemd-boot binary"
    if [ -n "$expected_target_ref" ]; then
      local target_composefs target_uki_sha
      target_composefs="$(python3 - "$run/install-proof.txt" <<'PY'
import json, pathlib, sys
line = pathlib.Path(sys.argv[1]).read_text().strip()
assert line.startswith('TILO_VM_INSTALL_PROOF_V2=')
print(json.loads(line.split('=', 1)[1])['installed_composefs_digest'])
PY
)" || die "guest did not report an installed target composefs digest"
      [[ "$target_composefs" =~ ^[0-9a-f]{128}$ ]] \
        || die "guest installed composefs digest is malformed"
      target_uki_sha="$(MTOOLS_SKIP_CHECK=1 mtype -i "$raw@@$((1537 * 1024 * 1024))" \
        "::/EFI/Linux/bootc/bootc_composefs-${target_composefs}.efi" \
        | sha256sum | awk '{print $1}')" \
        || die "installed target UKI is absent from the target ESP"
      python3 - "$run/install-proof.txt" "$target_uki_sha" <<'PY' \
        || die "guest proof UKI hash differs from host-read target ESP"
import json, pathlib, sys
proof = json.loads(pathlib.Path(sys.argv[1]).read_text().strip().split('=', 1)[1])
assert proof['target_uki_sha256'] == sys.argv[2]
PY
      printf 'target_uki_sha256=%s\n' "$target_uki_sha" \
        > "$out/installed-target-esp-readback.txt"
    fi
    if MTOOLS_SKIP_CHECK=1 mdir -i "$raw@@$((1537 * 1024 * 1024))" ::/loader/entries/tilo-windows.conf >/dev/null 2>&1 \
      || MTOOLS_SKIP_CHECK=1 mdir -i "$raw@@$((1537 * 1024 * 1024))" ::/loader/entries/tilo-linux-fedora.conf >/dev/null 2>&1; then
      die "guided Tilo EFI partition contains an invalid cross-ESP chainloader shortcut"
    fi
    printf 'boot_entries=foreign-esp-preserved;runtime_firmware_selection=not-fixtured;dedicated_tilo_esp_start_mib=1537;dedicated_tilo_boot_start_mib=3585;dedicated_tilo_root_start_mib=4609\n' \
      > "$out/alongside-boot-behavior.txt"
  fi
  printf 'mode=%s\nwindows_partition_sha256=%s\nlinux_partition_sha256=%s\nwindows_loader_sha256=%s\nlinux_loader_sha256=%s\n' \
    "$install_mode" "$(cat "$cycle_dir/windows.before.sha256")" "$(cat "$cycle_dir/linux.before.sha256")" \
    "$(cat "$cycle_dir/windows-loader.before.sha256")" "$(cat "$cycle_dir/linux-loader.before.sha256")" \
    > "$out/alongside-preservation.txt"
  printf 'fixture=synthetic-foreign-filesystems-with-controlled-efi-applications\nreal_windows_boot_verified=false\nreal_linux_distribution_boot_verified=false\n' \
    >> "$out/alongside-preservation.txt"
  rm -f -- "$raw"
}

shot_ocr() {
  local name="$1" image="$run/$name.png" text_file="$out/$name.txt"
  "$vm" shot "$name" >/dev/null
  tesseract "$image" stdout 2>/dev/null > "$text_file"
  printf '%s\n' "$image"
}

wait_screen() {
  local saved="$1" pattern="$2" attempts="${3:-60}" delay="${4:-5}" image i
  for i in $(seq 1 "$attempts"); do
    if image="$(shot_ocr observe 2>/dev/null)" \
      && grep -Eqi "$pattern" "$out/observe.txt"; then
      cp -f "$image" "$out/$saved.png"
      cp -f "$out/observe.txt" "$out/$saved.txt"
      return 0
    fi
    "$vm" status | grep -q '^running' || return 1
    sleep "$delay"
  done
  [ ! -f "$run/observe.png" ] || cp -f "$run/observe.png" "$out/$saved-timeout.png"
  [ ! -f "$out/observe.txt" ] || cp -f "$out/observe.txt" "$out/$saved-timeout.txt"
  return 1
}

die() { printf 'installer cycle: %s\n' "$*" >&2; exit 1; }

# Prepared-alongside reuses the foreign ESP, so the production helper creates
# same-filesystem systemd-boot entries for both controlled EFI applications.
# Open the real boot UI and try its displayed numeric shortcuts until both
# byte-preserved applications have painted their distinct marker. This remains
# a keyboard/framebuffer black-box proof: it does not edit the installed disk,
# inject a guest command, or infer execution from an entry file alone.
prove_prepared_foreign_boot_entries() {
  local index image marker current_menu menu_seen=0 windows_index="" linux_index=""
  [ "$install_mode" = prepared-alongside ] || return 0
  if [ "$software_tpm" -eq 1 ]; then
    printf '%s\n' \
      'fixture=controlled-unsigned-efi-applications' \
      'selection_verified=false' \
      'reason=unsigned-test-fixture-is-not-authorized-by-release-secure-boot-policy' \
      'real_windows_boot_verified=false' \
      'real_linux_distribution_boot_verified=false' \
      > "$out/foreign-boot-selection.txt"
    return 0
  fi

  phase=foreign-loader-proof
  for index in $(seq 1 9); do
    current_menu=0
    "$vm" boot-target --2d --no-tpm
    # The monitor is available before firmware handoff. Repeating Space makes
    # the systemd-boot menu deterministic even when loader.conf has timeout 0.
    for _ in $(seq 1 80); do
      "$vm" keys spc
      sleep 0.05
    done
    for _ in $(seq 1 20); do
      image="$(shot_ocr foreign-menu-observe 2>/dev/null || true)"
      if grep -Fqi 'Windows' "$out/foreign-menu-observe.txt" 2>/dev/null \
          && grep -Fqi 'Linux' "$out/foreign-menu-observe.txt" 2>/dev/null; then
        cp -f "$image" "$out/foreign-boot-menu-$index.png"
        cp -f "$out/foreign-menu-observe.txt" "$out/foreign-boot-menu-$index.txt"
        current_menu=1
        menu_seen=1
        break
      fi
      "$vm" status | grep -q '^running' || break
      "$vm" keys spc
      sleep 1
    done
    if [ "$current_menu" -eq 0 ]; then
      "$vm" stop >/dev/null 2>&1 || true
      continue
    fi

    # systemd-boot displays and accepts 1..9 for its first nine menu rows.
    "$vm" keys "$index"
    sleep 0.5
    "$vm" keys ret
    marker=""
    for _ in $(seq 1 20); do
      image="$(shot_ocr foreign-marker-observe 2>/dev/null || true)"
      if grep -Eqi 'TILO[[:space:]]+FOREIGN[[:space:]]+WINDOWS[[:space:]]+BOOT[[:space:]]+PASS' \
          "$out/foreign-marker-observe.txt" 2>/dev/null; then
        marker=windows
      elif grep -Eqi 'TILO[[:space:]]+FOREIGN[[:space:]]+LINUX[[:space:]]+BOOT[[:space:]]+PASS' \
          "$out/foreign-marker-observe.txt" 2>/dev/null; then
        marker=linux
      fi
      [ -z "$marker" ] || break
      "$vm" status | grep -q '^running' || break
      sleep 1
    done
    if [ -n "$marker" ]; then
      cp -f "$image" "$out/foreign-$marker-boot-marker.png"
      cp -f "$out/foreign-marker-observe.txt" "$out/foreign-$marker-boot-marker.txt"
      copy_evidence "foreign-$marker"
      if [ "$marker" = windows ]; then windows_index="$index"; else linux_index="$index"; fi
    fi
    "$vm" stop >/dev/null 2>&1 || true
    [ -z "$windows_index" ] || [ -z "$linux_index" ] || break
  done

  [ "$menu_seen" -eq 1 ] || die "could not open the installed systemd-boot menu"
  [ -n "$windows_index" ] || die "Windows fixture entry never executed from the installed boot menu"
  [ -n "$linux_index" ] || die "Linux fixture entry never executed from the installed boot menu"
  printf 'fixture=controlled-unsigned-efi-applications\nselection_verified=true\nmenu=installed-systemd-boot\nwindows_numeric_entry=%s\nlinux_numeric_entry=%s\nwindows_marker=TILO FOREIGN WINDOWS BOOT PASS\nlinux_marker=TILO FOREIGN LINUX BOOT PASS\nsecure_boot_verified=false\nreal_windows_boot_verified=false\nreal_linux_distribution_boot_verified=false\n' \
    "$windows_index" "$linux_index" > "$out/foreign-boot-selection.txt"
  printf 'controlled_foreign_efi_menu_boot_evidence=foreign-boot-selection.txt\n' \
    >> "$out/alongside-preservation.txt"
}

# Start from genuinely blank disk, TPM and UEFI variable-store state.
"$vm" init-cycle
[ "$install_mode" = erase ] || prepare_alongside_target

# Reproduce what the production Imager writes to a USB drive: preserve every
# byte of the released source image, reserve a 64 KiB device tail, and use the
# production profile encoder/read-back verifier to customize that tail. The
# live installer must discover this through its real pre-greetd extractor.
# The CI path holds only the appended profile in a qcow2 overlay, avoiding two
# full-disk conversions while keeping the released source immutable.
source_hash_before="$(sha256sum "$medium" | awk '{print $1}')"
profiled_medium="$cycle_dir/profiled-medium.qcow2"
if [ "$build_profile_encoder" -eq 0 ]; then
  # Recheck immediately before execution, after potentially long fixture setup.
  [ "$(sha256sum "$encoder" | awk '{print $1}')" = "$encoder_sha" ] \
    || die "profile encoder changed after admission"
fi
profile_args=(
  --hostname cycle-tilo \
  --username "$TILO_USER" \
  --full-name 'Cycle User' \
  --locale en_US.UTF-8 \
  --keyboard us \
  --timezone UTC \
  --filesystem btrfs
)
if [ "$build_profile_encoder" -eq 1 ]; then
  # The local cargo-run mode has a multiword encoder command. Retain its
  # existing raw-file fixture until its runner can pin a standalone binary.
  profiled_raw="$cycle_dir/profiled-medium.raw"
  qemu-img convert -f qcow2 -O raw "$medium" "$profiled_raw"
  truncate -s "+$((64 * 1024))" "$profiled_raw"
  "${encoder_command[@]}" --target "$profiled_raw" "${profile_args[@]}"
  qemu-img convert -f raw -O qcow2 "$profiled_raw" "$profiled_medium"
  rm -f -- "$profiled_raw"
else
  python3 "$dev_dir/profile-installer-qcow2.py" \
    "$medium" "$profiled_medium" "$encoder" "${profile_args[@]}"
fi
[ "$(sha256sum "$medium" | awk '{print $1}')" = "$source_hash_before" ] \
  || die "customization modified the released installer medium"
export TILO_INSTALLER_DISK="$profiled_medium"
printf 'medium=%s\ncustomized_medium=%s\narchitecture=%s\nuser=%s\nhostname=cycle-tilo\nunlock=%s\ninstall_mode=%s\nstorage=%s\ngraphics=%s\nprofile_encoder_sha256=%s\n' \
  "$medium" "$profiled_medium" "$TILO_VM_ARCH" "$TILO_USER" "$unlock" "$install_mode" "$storage" "$graphics" "$encoder_sha" \
  > "$out/scenario.txt"

proof_arg=()
[ -z "$expected_target_ref" ] || proof_arg=(--install-proof)
if [ "$software_tpm" -eq 1 ]; then
  "$vm" boot-installer "$display_arg" --tpm "${proof_arg[@]}"
else
  "$vm" boot-installer "$display_arg" --no-tpm "${proof_arg[@]}"
fi
console="$("$vm" console)"
ic() { python3 "$dev_dir/installer-console.py" --socket "$console" "$@"; }
field() { python3 "$dev_dir/installer-console.py" field "$@"; }

# The installer is driven through the actions it publishes: the same
# addresses its buttons invoke, reached over the console channel that only
# this VM's command line attaches (a virtio-serial port; see vm.sh console).
ic wait installer --timeout 600 --save "$out/01-disk.json" \
  || die "profiled live installer did not start at Disk"
if [ "$selinux_audit_ssh" -eq 1 ]; then
  # Opt-in --dev-login medium only; release media have no guest login.
  "$dev_dir/installer-selinux-audit.sh" assert-ready "$out" \
    || die 'live installer is not enforcing with an active, readable audit stream'
fi
[ "$(field "$out/01-disk.json" /step)" = disk ] \
  || die "profiled live installer did not start at Disk"
field "$out/01-disk.json" /status | grep -Eqi 'setup choices are ready' \
  || die "live installer did not acknowledge the embedded setup profile"
"$vm" shot 01-disk >/dev/null && cp -f "$run/01-disk.png" "$out/01-disk.png" || true

# Select the method, refresh, then the only selectable disk (/dev/vdb). The
# live /dev/vda is refused by the shipped disk identity rules.
method=erase
[ "$install_mode" = erase ] || method=alongside
ic invoke set-method@installer.storage "{\"method\":\"$method\"}" \
  || die "installer refused the $method install method"
ic invoke refresh@installer.storage || die "installer could not refresh disks"
ic invoke select@installer.disk:/dev/vdb || die "installer did not offer the blank target"
ic invoke next@installer || die "installer did not select the blank target"
ic wait-step account --save "$out/02-account.json" \
  || die "installer did not select the blank target"
[ "$(field "$out/02-account.json" /account/username)" = "$TILO_USER" ] \
  || die "embedded username did not reach the account screen"
if [ "$software_tpm" -eq 1 ]; then
  [ "$(field "$out/02-account.json" /account/secret_policy)" = tpm-pin ] \
    || die "software TPM was attached, but the live and target boot chain did not pass verified-boot preflight"
fi
"$vm" shot 02-account >/dev/null && cp -f "$run/02-account.png" "$out/02-account.png" || true

# The startup secret travels from the environment to the console argument;
# it is never on a command line and never in a reply.
ic invoke set-passphrase@installer.account \
  --secret passphrase=TILO_PASS --secret confirmation=TILO_PASS \
  || die "installer rejected the configured startup secret"
ic invoke next@installer || die "installer rejected the configured startup secret"
ic wait-step review --save "$out/03-review.json" \
  || die "installer rejected the configured startup secret"
field "$out/03-review.json" /options/hostname | grep -Eqi 'cycle.?tilo' \
  || die "Review did not show the configured hostname"
"$vm" shot 03-review >/dev/null && cp -f "$run/03-review.png" "$out/03-review.png" || true

# Exact destructive confirmation -> recovery-key promise -> Begin Install.
# Prepared alongside formats only partition 4; guided alongside confirms the
# reviewed free-space operation with the whole disk because it appends p4+p5.
# `begin` is published with a typed-phrase confirmation contract: the phrase
# is the object it formats, and anything else is refused before any work.
confirmation=/dev/vdb
[ "$install_mode" != prepared-alongside ] || confirmation=/dev/vdb5
[ "$(field "$out/03-review.json" /storage/confirmation_target)" = "$confirmation" ] \
  || die "Review names a different destructive target than this layout formats"
ic invoke set-recovery-promise@installer.review '{"acknowledged":true}' \
  || die "installer refused the recovery-key promise"
if ic invoke begin@installer.review '{"confirmation_phrase":"/dev/vda"}' 2>/dev/null; then
  die "installer began an install confirmed with the wrong device"
fi
ic invoke begin@installer.review "{\"confirmation_phrase\":\"$confirmation\"}" \
  || die "installer refused the confirmed install"

# Completion is read from the installer's own state. That state never holds
# the one-time recovery key (only whether it exists and was saved), so the
# installer phase keeps no screenshot of the Finish screen and no serial log.
max_wait="${TILO_INSTALLER_CYCLE_MAX_INSTALL_SECONDS:-3600}"
install_seconds="$(ic wait-install --timeout "$max_wait" --save "$out/04-install.json")" \
  || die "installer did not reach its Finish screen"
printf 'finish_screen_wait=%s\n' "$install_seconds" > "$out/install-timing.txt"
[ "$(field "$out/04-install.json" /recovery_key/available)" = true ] \
  || die "installer finished without generating a recovery key"
if [ "$selinux_audit_ssh" -eq 1 ]; then
  "$dev_dir/installer-selinux-audit.sh" assert-clean "$out" \
    || die 'live installer recorded an AVC, lost audit records, or audit inspection failed'
fi

# Copy the key into the ephemeral live clipboard, acknowledge the disposable
# test target, then Power off so the next boot contains only the produced
# target. The reply to power-off may be cut off by the guest going down.
ic invoke copy@installer.recovery || die "installer could not copy the recovery key"
ic invoke confirm-saved@installer.recovery '{"saved":true}' \
  || die "installer refused the saved-key acknowledgement"
ic invoke power-off@installer.power || true
for _ in $(seq 1 60); do
  "$vm" status | grep -q '^stopped' && break
  sleep 2
done
"$vm" status | grep -q '^stopped' || die "installer Finish action did not power off"
copy_evidence installer
"$vm" stop >/dev/null 2>&1 || true

# Keep installation/preservation evidence independently of later desktop/OOBE
# acceptance. A later failure must not erase what this exact target already
# proved. Verify again after the cold target boot before issuing overall PASS.
if [ "$install_mode" != erase ]; then
  verify_alongside_target
  cp "$out/alongside-preservation.txt" "$out/alongside-preservation-before-target-boot.txt"
fi

if [ -n "$expected_target_ref" ]; then
  proof_file="$run/install-proof.txt"
  [ -f "$proof_file" ] && [ ! -L "$proof_file" ] \
    || die "VM guest did not emit its dedicated install proof"
  python3 - "$proof_file" "$expected_target_ref" "$out/guest-install-proof.json" <<'PY'
import json
from pathlib import Path
import re
import sys

lines = Path(sys.argv[1]).read_text(encoding='utf-8').splitlines()
if len(lines) != 1 or not lines[0].startswith('TILO_VM_INSTALL_PROOF_V2='):
    raise SystemExit('VM guest did not emit exactly one v2 install proof')
proof = json.loads(lines[0].split('=', 1)[1])
expected = sys.argv[2]
digest = expected.split('@', 1)[1]
selected = proof.get('selected_target', {})
if (proof.get('schema') != 'tilo.vm-install-image-proof.v2'
        or proof.get('architecture') != 'arm64'
        or selected.get('target_ref') != expected
        or selected.get('target_digest') != digest
        or proof.get('installed_image_digest') != digest
        or proof.get('installed_update_origin') !=
        'ostree-unverified-image:docker://' + selected.get('update_origin', '')
        or proof.get('installed_composefs_digest') != proof.get('composefs_digest')
        or not re.fullmatch('[0-9a-f]{128}', proof.get('installed_composefs_digest', ''))
        or not re.fullmatch('[0-9a-f]{64}', proof.get('target_uki_sha256', ''))
        or set(proof.get('partuuids', {})) != {'efi', 'boot', 'root'}):
    raise SystemExit('VM install proof differs from independently pinned signed target')
Path(sys.argv[3]).write_text(json.dumps(proof, sort_keys=True, indent=2) + '\n')
PY
fi
if [ "$install_only" -eq 1 ]; then
  printf 'PASS_INSTALL_ONLY\n' > "$out/verdict.txt"
  exit 0
fi

phase=target
target_boot_started="$(date +%s)"
if [ "$software_tpm" -eq 1 ]; then
  "$vm" boot-target "$display_arg" --tpm
else
  "$vm" boot-target "$display_arg" --no-tpm
fi

# The sealed boot has no serial-console test instrumentation.  The installed
# target consumes its one-boot greetd handoff here, so its first visible surface
# is OOBE rather than the password greeter.  Send the disk secret only during
# the early unlock window; do not keep retrying after an OOBE surface could own
# keyboard focus.
oobe_started=0
for attempt in $(seq 1 60); do
  if image="$(shot_ocr target-observe 2>/dev/null)" \
    && grep -Eqi 'Create[[:space:]]+your[[:space:]]+local' "$out/target-observe.txt"; then
    cp -f "$image" "$out/07-first-boot-oobe.png"
    cp -f "$out/target-observe.txt" "$out/07-first-boot-oobe.txt"
    target_oobe_seen="$(date +%s)"
    printf 'cold_boot_to_oobe_seconds=%s\n' \
      "$((target_oobe_seen - target_boot_started))" > "$out/boot-timing.txt"
    oobe_started=1
    break
  fi
  "$vm" status | grep -q '^running' || die "installed target exited before unlock"
  if [ "$attempt" -ge 2 ] && [ "$attempt" -le 3 ]; then
    "$vm" type "$TILO_PASS"
    "$vm" keys ret
  fi
  sleep 5
done
[ "$oobe_started" -eq 1 ] || die "first installed boot did not reach direct OOBE"
wait_screen 08-oobe-intro 'Create[[:space:]]+your[[:space:]]+local' 10 1 \
  || die "first installed boot did not preserve the OOBE introduction"

# Use the real first-use flow: Create identity -> acknowledge no recovery ->
# Create. OCR of the backup screen is the barrier for the async mint.
"$vm" keys tab ret
sleep 2
"$vm" keys spc tab tab ret
wait_screen 09-oobe-backup 'Back[[:space:]]+up[[:space:]]+your[[:space:]]+Tilo[[:space:]]+identity' 120 2 \
  || die "OOBE did not create the local/device identity chain"

# Simulate power loss at the durable identity/backup boundary.  The following
# cold boot must resume this exact OOBE screen without recreating the identity.
# It also proves that the initial_session cleanup survived an interrupted first
# session before we ever reach the desktop.
"$vm" stop
if [ "$software_tpm" -eq 1 ]; then
  "$vm" boot-target "$display_arg" --tpm
else
  "$vm" boot-target "$display_arg" --no-tpm
fi
wait_screen 10-oobe-backup-resumed 'Back[[:space:]]+up[[:space:]]+your[[:space:]]+Tilo[[:space:]]+identity' 120 2 \
  || die "OOBE did not resume after an interrupted first boot"

# A fresh identity must either save and verify its encrypted recovery material
# or explicitly acknowledge that losing this device loses the identity. This
# disposable cycle exercises the latter: from the first passphrase field, three
# Tabs reach the mandatory acknowledgement, then Continue becomes available.
"$vm" keys tab tab tab spc tab ret
wait_screen 11-oobe-ready 'Your[[:space:]]+identity[[:space:]]+is[[:space:]]+ready' 60 2 \
  || die "OOBE did not require and accept the backup-loss acknowledgement"

# Ready is a replacement surface and focuses Take a quick tour. Traverse every
# compositor-owned tutorial page and OCR each replacement surface so a tagged
# release cannot ship a dead or unreachable tutorial behind healthy OOBE.
"$vm" keys ret
wait_screen 12-tour-move 'Everything[[:space:]]+is[[:space:]]+close[[:space:]]+at[[:space:]]+hand' 60 2 \
  || die "desktop tutorial did not open its navigation page"

# Each tutorial page reseats focus on Skip tour. Back and then Continue are the
# next two controls, so this drives the same keyboard route a user receives.
"$vm" keys tab tab ret
wait_screen 13-tour-arrange 'A[[:space:]]+place[[:space:]]+for[[:space:]]+each[[:space:]]+kind[[:space:]]+of[[:space:]]+work' 60 2 \
  || die "desktop tutorial did not advance to its organisation page"
"$vm" keys tab tab ret
wait_screen 14-tour-share 'Sharing[[:space:]]+stays[[:space:]]+deliberate' 60 2 \
  || die "desktop tutorial did not advance to its collaboration page"
"$vm" keys tab tab ret
wait_screen 15-tour-start 'Your[[:space:]]+first[[:space:]]+five[[:space:]]+minutes' 60 2 \
  || die "desktop tutorial did not advance to its first-steps page"
"$vm" keys tab tab ret
sleep 8

# The default installed workspace focuses its shipped Terminal tile. Prove the
# configured hostname from that user-facing surface; no guest control channel
# or privileged observer is introduced for the test.
"$vm" type hostname
"$vm" keys ret
wait_screen 16-desktop-config 'cycle.?tilo' 60 2 \
  || die "desktop terminal did not report the installed hostname"

# Exercise the installed Browser in this same target-only cold boot. The page
# is served by the isolated QEMU worker through user networking's local gateway;
# it contains no external resources and therefore proves the browser engine can
# launch and paint useful content without Internet or registry access. Keep the
# exact fixture digest and visible framebuffer/OCR as durable evidence.
browser_fixture_port="${TILO_INSTALLER_CYCLE_BROWSER_PORT:-18765}"
[[ "$browser_fixture_port" =~ ^[0-9]+$ ]] \
  && [ "$browser_fixture_port" -ge 1024 ] && [ "$browser_fixture_port" -le 65535 ] \
  || die "browser fixture port must be between 1024 and 65535"
browser_fixture_dir="$run/browser-fixture"
install -d -m 0700 "$browser_fixture_dir"
cat > "$browser_fixture_dir/index.html" <<'EOF'
<!doctype html>
<meta charset="utf-8">
<title>Tilo browser offline acceptance</title>
<style>body{background:#183245;color:#fff;font:32px sans-serif;padding:48px}h1{font-size:64px}</style>
<h1>Tilo browser offline ready</h1>
<p>This page came from the local ARM VM acceptance fixture.</p>
EOF
browser_fixture_sha256="$(sha256sum "$browser_fixture_dir/index.html" | awk '{print $1}')"
cp "$browser_fixture_dir/index.html" "$out/browser-offline-fixture.html"
browser_fixture_url="http://10.0.2.2:$browser_fixture_port/index.html"
printf 'url=%s\nsha256=%s\ntransport=qemu-user-local-gateway\nexternal_references=none\n' \
  "$browser_fixture_url" "$browser_fixture_sha256" > "$out/browser-offline-fixture.txt"
python3 -m http.server "$browser_fixture_port" --bind 0.0.0.0 \
  --directory "$browser_fixture_dir" > "$out/browser-fixture-server.log" 2>&1 &
browser_fixture_server=$!
fixture_ready=0
for _ in $(seq 1 50); do
  kill -0 "$browser_fixture_server" 2>/dev/null \
    || die "local browser fixture server exited during startup"
  if python3 -c 'import hashlib,sys,urllib.request; body=urllib.request.urlopen(sys.argv[1],timeout=1).read(); assert hashlib.sha256(body).hexdigest()==sys.argv[2]' \
      "http://127.0.0.1:$browser_fixture_port/index.html" "$browser_fixture_sha256" 2>/dev/null; then
    fixture_ready=1
    break
  fi
  sleep 0.1
done
[ "$fixture_ready" -eq 1 ] || die "local browser fixture server did not become ready"

# Super+Space opens the compositor's global palette. Launch Browser through the
# same installed-package route a user receives, focus its address field, then
# navigate to the local fixture. The final OCR assertion is page content, not
# browser chrome or a process check, so startup without a rendered web view
# cannot pass.
"$vm" keys meta_l-spc
sleep 2
"$vm" type browser
wait_screen 16-browser-picker 'Browser' 60 1 \
  || die "installed Browser was absent from the desktop app picker"
"$vm" keys ret
wait_screen 17-browser-open 'Search[[:space:]]+or[[:space:]]+enter[[:space:]]+address' 120 2 \
  || die "installed Browser did not launch into the desktop session"
"$vm" keys ctrl-l ctrl-a backspace
"$vm" type "$browser_fixture_url"
"$vm" keys ret
wait_screen 18-browser-offline 'Tilo[[:space:]]+browser[[:space:]]+offline[[:space:]]+ready' 120 2 \
  || die "installed Browser did not render the local offline acceptance page"

# A second cold boot is the user-visible proof that the handoff was consumed:
# greetd must display normal authentication and the same password must reopen
# the completed account.  This is intentionally after OOBE so neither recovery
# state nor a normal session can hide a persistent autologin defect.
"$vm" stop
target_reboot_started="$(date +%s)"
if [ "$software_tpm" -eq 1 ]; then
  "$vm" boot-target "$display_arg" --tpm
else
  "$vm" boot-target "$display_arg" --no-tpm
fi
wait_screen 19-reboot-greeter 'Welcome[[:space:]]+back|Sign[[:space:]]+in' 120 2 \
  || die "second installed boot bypassed the password greeter"
target_reboot_greeter_seen="$(date +%s)"
printf 'cold_reboot_to_greeter_seconds=%s\n' \
  "$((target_reboot_greeter_seen - target_reboot_started))" >> "$out/boot-timing.txt"
"$vm" login --passcode-only "$TILO_PASS"
wait_screen 20-reboot-login 'cycle.?tilo' 120 2 \
  || die "password authentication did not reopen the completed account"

copy_evidence target
[ "$install_mode" = erase ] || {
  "$vm" stop
  verify_alongside_target
  prove_prepared_foreign_boot_entries
}
printf 'PASS\n' > "$out/verdict.txt"
