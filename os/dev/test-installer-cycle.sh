#!/usr/bin/env bash
# Release-grade live-medium -> encrypted target -> first-login cycle.
#
# The only guest inputs are QEMU keyboard events sent through vm.sh. Screen
# transitions are observed from the real framebuffer with OCR; no debug socket,
# SSH service, shot mode, backend invocation, or target-disk modification is
# used. The no-TPM path proves that the passphrase enrolled by the graphical
# installer unlocks early userspace after a cold power cycle.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
vm="$dev_dir/vm.sh"
medium="${1:?usage: test-installer-cycle.sh INSTALLER_QCOW2 [--out DIRECTORY] [--prepared-alongside]}"
shift
out=""
install_mode=erase
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:-}"; shift 2;;
    --prepared-alongside) install_mode=prepared-alongside; shift;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2;;
  esac
done
[ -f "$medium" ] || { printf 'installer medium not found: %s\n' "$medium" >&2; exit 1; }
command -v tesseract >/dev/null 2>&1 \
  || { printf 'tesseract is required for framebuffer verification\n' >&2; exit 1; }

out="${out:-$PWD/installer-cycle-artifacts}"
export QOS_VM_CYCLE="${QOS_VM_CYCLE:-installer-cycle-$$}"
storage="${QOS_INSTALLER_CYCLE_STORAGE:-$(dirname "$out")/.$(basename "$out")-storage-$QOS_VM_CYCLE}"
run="${QOS_RUN:-${XDG_RUNTIME_DIR:-/tmp}/qos-vm-$QOS_VM_CYCLE}"
cycle_dir="$storage/vm-cycles/$QOS_VM_CYCLE"
mkdir -p "$out" "$storage"

export QOS_INSTALLER_OUTPUT="$storage"
export QOS_RUN="$run"
export QOS_INSTALLER_DISK="$medium"
export QOS_INSTALLER_WORK_DISK="$cycle_dir/installer-work.qcow2"
export QOS_TARGET_DISK="$cycle_dir/target.qcow2"
export QOS_TARGET_SIZE="${QOS_TARGET_SIZE:-40G}"
export QOS_USER="${QOS_INSTALLER_CYCLE_USER:-cycleuser}"
export QOS_PASS="${QOS_INSTALLER_CYCLE_PASSPHRASE:-cycle-passphrase-2026}"
export QOS_BOOT_WAIT_ATTEMPTS="${QOS_BOOT_WAIT_ATTEMPTS:-600}"

phase=installer
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
      '::qos-recovery-key::|RECOVERY KEY.*(write|save|off-device)|[bcdfghjkmnpqrstvwxyz]{8}(-[bcdfghjkmnpqrstvwxyz]{8}){7}' \
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
printf '%s\n%s\n' '::qos-recovery-key::artifact-canary' \
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
  rm -f -- "$run/private-observe.png" "$run/private-observe.ppm"
  copy_evidence "$phase" || true
  "$vm" stop >/dev/null 2>&1 || true
  if command -v qemu-img >/dev/null 2>&1 && [ -f "$QOS_TARGET_DISK" ]; then
    qemu-img info "$QOS_TARGET_DISK" > "$out/target-qemu-img-info.txt" 2>&1 || true
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
  local linux_loader
  for tool in qemu-img sfdisk mkfs.vfat mkfs.ntfs mkfs.ext4 mcopy mmd ntfscp debugfs; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required for prepared-alongside cycle"
  done
  # MiB-aligned 40 GiB GPT: shared 2 GiB ESP, Windows and Linux sentinels,
  # then a signature-free >=32 GiB qOS root prepared for the real installer.
  truncate -s 40G "$raw"
  sfdisk "$raw" >/dev/null <<'EOF'
label: gpt
start=2048, size=4194304, type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b, uuid=10000000-0000-4000-8000-000000000001, name="shared EFI"
start=4196352, size=1048576, type=ebd0a0a2-b9e5-4433-87c0-68b6b72699c7, uuid=20000000-0000-4000-8000-000000000002, name="Windows sentinel"
start=5244928, size=1048576, type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=30000000-0000-4000-8000-000000000003, name="Linux sentinel"
start=6293504, size=77590000, type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=40000000-0000-4000-8000-000000000004, name="qOS prepared root"
EOF
  truncate -s 2G "$esp"; mkfs.vfat -n SHARED-EFI "$esp" >/dev/null
  mmd -i "$esp" ::/EFI ::/EFI/Microsoft ::/EFI/Microsoft/Boot ::/EFI/Fedora
  printf 'windows-loader-sentinel\n' > "$cycle_dir/bootmgfw.efi"
  case "$QOS_VM_ARCH" in x86_64) linux_loader=shimx64.efi;; aarch64) linux_loader=shimaa64.efi;; esac
  printf 'linux-loader-sentinel\n' > "$cycle_dir/$linux_loader"
  mcopy -i "$esp" "$cycle_dir/bootmgfw.efi" ::/EFI/Microsoft/Boot/bootmgfw.efi
  mcopy -i "$esp" "$cycle_dir/$linux_loader" "::/EFI/Fedora/$linux_loader"
  truncate -s 512M "$windows"; mkfs.ntfs -F -L WINDOWS-SENTINEL "$windows" >/dev/null
  truncate -s 512M "$linux"; mkfs.ext4 -q -F -L LINUX-SENTINEL "$linux"
  printf 'windows-filesystem-preservation-sentinel\n' > "$cycle_dir/windows-sentinel.txt"
  printf 'linux-filesystem-preservation-sentinel\n' > "$cycle_dir/linux-sentinel.txt"
  ntfscp "$windows" "$cycle_dir/windows-sentinel.txt" /preservation.txt
  debugfs -w -R "write $cycle_dir/linux-sentinel.txt /preservation.txt" "$linux" >/dev/null
  sha256sum "$windows" | awk '{print $1}' > "$cycle_dir/windows.before.sha256"
  sha256sum "$linux" | awk '{print $1}' > "$cycle_dir/linux.before.sha256"
  dd if="$esp" of="$raw" bs=1M seek=1 conv=notrunc,sparse status=none
  dd if="$windows" of="$raw" bs=1M seek=2049 conv=notrunc,sparse status=none
  dd if="$linux" of="$raw" bs=1M seek=2561 conv=notrunc,sparse status=none
  qemu-img convert -f raw -O qcow2 "$raw" "$QOS_TARGET_DISK"
  rm -f -- "$raw" "$esp" "$windows" "$linux" "$cycle_dir/bootmgfw.efi" \
    "$cycle_dir/$linux_loader" "$cycle_dir/windows-sentinel.txt" "$cycle_dir/linux-sentinel.txt"
}

verify_alongside_target() {
  local raw="$cycle_dir/installed-alongside.raw" actual
  qemu-img convert -f qcow2 -O raw "$QOS_TARGET_DISK" "$raw"
  actual="$(dd if="$raw" bs=512 skip=4196352 count=1048576 status=none | sha256sum | awk '{print $1}')"
  [ "$actual" = "$(cat "$cycle_dir/windows.before.sha256")" ] \
    || die "Windows filesystem bytes changed during alongside install"
  actual="$(dd if="$raw" bs=512 skip=5244928 count=1048576 status=none | sha256sum | awk '{print $1}')"
  [ "$actual" = "$(cat "$cycle_dir/linux.before.sha256")" ] \
    || die "Linux filesystem bytes changed during alongside install"
  MTOOLS_SKIP_CHECK=1 mtype -i "$raw@@1048576" ::/loader/entries/qos-windows.conf \
    | grep -Fq 'title Windows' || die "Windows chainloader entry is missing"
  MTOOLS_SKIP_CHECK=1 mtype -i "$raw@@1048576" ::/loader/entries/qos-linux-fedora.conf \
    | grep -Fq 'title Linux (Fedora)' || die "Linux chainloader entry is missing"
  printf 'windows_partition_sha256=%s\nlinux_partition_sha256=%s\nboot_entries=windows,linux-fedora\n' \
    "$(cat "$cycle_dir/windows.before.sha256")" "$(cat "$cycle_dir/linux.before.sha256")" \
    > "$out/alongside-preservation.txt"
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

# Observe a screen which contains the recovery key without ever retaining its
# framebuffer or OCR output. The data lives only in process memory and the
# private VM runtime, and every path deletes it before returning.
wait_private_screen() {
  local pattern="$1" attempts="${2:-720}" delay="${3:-5}" image text i
  for i in $(seq 1 "$attempts"); do
    image="$run/private-observe.png"
    if "$vm" shot private-observe >/dev/null 2>&1; then
      text="$(tesseract "$image" stdout 2>/dev/null || true)"
      rm -f -- "$image" "$run/private-observe.ppm"
      if printf '%s\n' "$text" | grep -Eqi "$pattern"; then
        text=""
        printf 'finish_screen_wait_seconds=%s\n' "$((i * delay))" \
          > "$out/install-timing.txt"
        return 0
      fi
      text=""
    else
      rm -f -- "$image" "$run/private-observe.ppm"
    fi
    "$vm" status | grep -q '^running' || return 1
    sleep "$delay"
  done
  return 1
}

die() { printf 'installer cycle: %s\n' "$*" >&2; exit 1; }

# Start from genuinely blank disk, TPM and UEFI variable-store state.
"$vm" init-cycle
[ "$install_mode" != prepared-alongside ] || prepare_alongside_target

# Reproduce what the production Imager writes to a USB drive: preserve every
# byte of the released source image, reserve a 64 KiB device tail, and use the
# production profile encoder/read-back verifier to customize that tail. The
# live installer must discover this through its real pre-greetd extractor.
# Converting the regular-file fixture back to qcow2 makes it bootable by the
# same VM path without ever mutating the release artifact.
source_hash_before="$(sha256sum "$medium" | awk '{print $1}')"
profiled_raw="$cycle_dir/profiled-medium.raw"
profiled_medium="$cycle_dir/profiled-medium.qcow2"
qemu-img convert -f qcow2 -O raw "$medium" "$profiled_raw"
truncate -s "+$((64 * 1024))" "$profiled_raw"
cargo run -q -p imager --locked --example embed-profile -- \
  --target "$profiled_raw" \
  --hostname cycle-qos \
  --username "$QOS_USER" \
  --full-name 'Cycle User' \
  --locale en_US.UTF-8 \
  --keyboard us \
  --timezone UTC \
  --filesystem btrfs
qemu-img convert -f raw -O qcow2 "$profiled_raw" "$profiled_medium"
rm -f -- "$profiled_raw"
[ "$(sha256sum "$medium" | awk '{print $1}')" = "$source_hash_before" ] \
  || die "customization modified the released installer medium"
export QOS_INSTALLER_DISK="$profiled_medium"
printf 'medium=%s\ncustomized_medium=%s\narchitecture=%s\nuser=%s\nhostname=cycle-qos\nmode=profiled-no-tpm-passphrase\nstorage=%s\n' \
  "$medium" "$profiled_medium" "$QOS_VM_ARCH" "$QOS_USER" "$install_mode" > "$out/scenario.txt"

"$vm" boot-installer --gl --no-tpm
wait_screen 01-disk 'Install[[:space:]]+qOS' 120 5 \
  || die "profiled live installer did not start at Disk"
grep -Eqi 'setup choices are ready' "$out/01-disk.txt" \
  || die "live installer did not acknowledge the embedded setup profile"

# Select Alongside when requested, then Refresh -> the only selectable row
# (/dev/vdb). The live
# /dev/vda row is disabled by the shipped disk identity rules.
[ "$install_mode" != prepared-alongside ] || "$vm" keys right
"$vm" keys tab tab ret
sleep 2
"$vm" keys tab ret
wait_screen 02-account 'Create[[:space:]]+your[[:space:]]+account' 60 3 \
  || die "installer did not select the blank target"
grep -Fqi -- "$QOS_USER" "$out/02-account.txt" \
  || die "embedded username did not reach the account screen"

"$vm" keys tab
"$vm" type "$QOS_PASS"
"$vm" keys tab
"$vm" type "$QOS_PASS"
"$vm" keys tab tab ret
wait_screen 03-review 'Review[[:space:]]+your[[:space:]]+choices' 60 3 \
  || die "installer rejected the no-TPM passphrase account"
grep -Eqi 'cycle.?qos' "$out/03-review.txt" \
  || die "Review did not show the configured hostname"

# Exact destructive confirmation -> recovery-key promise -> Begin Install.
# Prepared alongside formats only partition 4; typing the whole disk would be
# rejected by both the Review gate and the privileged --confirm-root binding.
confirmation=/dev/vdb
[ "$install_mode" != prepared-alongside ] || confirmation=/dev/vdb4
"$vm" type "$confirmation"
"$vm" keys tab spc tab tab ret

# The production Finish title is the completion oracle. Its screen also holds
# the one-time recovery key, so OCR is memory-only and its framebuffer is
# deleted immediately rather than copied into the artifact directory.
max_wait="${QOS_INSTALLER_CYCLE_MAX_INSTALL_SECONDS:-3600}"
wait_private_screen 'qOS[[:space:]]+is[[:space:]]+installed|Installation[[:space:]]+complete' \
  "$((max_wait / 5))" 5 || die "installer did not reach its Finish screen"

# Completion explicitly focuses the first recovery action. Copy the key into
# the ephemeral live clipboard, acknowledge the disposable test target, then
# choose Power off so the next boot contains only the produced target.
"$vm" keys ret tab spc tab tab ret
for _ in $(seq 1 60); do
  "$vm" status | grep -q '^stopped' && break
  sleep 2
done
"$vm" status | grep -q '^stopped' || die "graphical Finish action did not power off"
copy_evidence installer
"$vm" stop >/dev/null 2>&1 || true

phase=target
"$vm" boot-target --gl --no-tpm

# The sealed boot has no serial-console test instrumentation. Type only after
# firmware/systemd-boot have had time to hand off, and stop as soon as OCR sees
# the real greeter so a late retry cannot contaminate its username field.
unlocked=0
for attempt in $(seq 1 60); do
  if image="$(shot_ocr target-observe 2>/dev/null)" \
    && grep -Eqi 'Welcome[[:space:]]+back|Sign[[:space:]]+in' "$out/target-observe.txt"; then
    cp -f "$image" "$out/07-target-greeter.png"
    cp -f "$out/target-observe.txt" "$out/07-target-greeter.txt"
    unlocked=1
    break
  fi
  "$vm" status | grep -q '^running' || die "installed target exited before unlock"
  if [ "$attempt" -ge 2 ]; then
    "$vm" type "$QOS_PASS"
    "$vm" keys ret
  fi
  sleep 10
done
[ "$unlocked" -eq 1 ] || die "installed target did not unlock into the greeter"

# Successful graphical authentication proves the installer-created account and
# its password hash. A fresh account must then enter trusted compositor OOBE.
"$vm" login --passcode-only "$QOS_PASS"
wait_screen 08-oobe-intro 'Create[[:space:]]+your[[:space:]]+local' 120 5 \
  || die "first graphical login did not enter qOS identity setup"

# Use the real first-use flow: Create identity -> acknowledge no recovery ->
# Create. OCR of the completion screen is the barrier for the async mint.
"$vm" keys tab ret
sleep 2
"$vm" keys spc tab tab ret
wait_screen 09-oobe-ready 'Your[[:space:]]+identity[[:space:]]+is[[:space:]]+ready' 120 2 \
  || die "OOBE did not create the local/device identity chain"

# Done is a replacement surface and focuses Save backup. Two Tabs reach Start
# tour without opening the file picker. Traverse all three compositor-owned
# tutorial pages and OCR each replacement surface so a tagged release cannot
# ship a dead or unreachable tutorial behind an otherwise healthy OOBE.
"$vm" keys tab tab ret
wait_screen 10-tour-move 'Everything[[:space:]]+is[[:space:]]+close[[:space:]]+at[[:space:]]+hand' 60 2 \
  || die "desktop tutorial did not open its navigation page"

# Each tutorial page reseats focus on Skip tour. Back and then Continue are the
# next two controls, so this drives the same keyboard route a user receives.
"$vm" keys tab tab ret
wait_screen 11-tour-arrange 'A[[:space:]]+place[[:space:]]+for[[:space:]]+each[[:space:]]+kind[[:space:]]+of[[:space:]]+work' 60 2 \
  || die "desktop tutorial did not advance to its organisation page"
"$vm" keys tab tab ret
wait_screen 12-tour-share 'Sharing[[:space:]]+stays[[:space:]]+deliberate' 60 2 \
  || die "desktop tutorial did not advance to its collaboration page"
"$vm" keys tab tab ret
sleep 8

# The default installed workspace focuses its shipped Terminal tile. Prove the
# configured hostname from that user-facing surface; no guest control channel
# or privileged observer is introduced for the test.
"$vm" type hostname
"$vm" keys ret
wait_screen 13-desktop-config 'cycle.?qos' 60 2 \
  || die "desktop terminal did not report the installed hostname"

copy_evidence target
[ "$install_mode" != prepared-alongside ] || { "$vm" stop; verify_alongside_target; }
printf 'PASS\n' > "$out/verdict.txt"
