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
medium="${1:?usage: test-installer-cycle.sh INSTALLER_QCOW2 [--out DIRECTORY]}"
shift
out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:-}"; shift 2;;
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
printf 'medium=%s\ncustomized_medium=%s\narchitecture=%s\nuser=%s\nhostname=cycle-qos\nmode=profiled-no-tpm-passphrase\n' \
  "$medium" "$profiled_medium" "$QOS_VM_ARCH" "$QOS_USER" > "$out/scenario.txt"

"$vm" boot-installer --gl --no-tpm
wait_screen 01-disk 'Install[[:space:]]+qOS' 120 5 \
  || die "profiled live installer did not start at Disk"
grep -Eqi 'setup choices are ready' "$out/01-disk.txt" \
  || die "live installer did not acknowledge the embedded setup profile"

# Erase (default) -> Refresh -> the only selectable row (/dev/vdb). The live
# /dev/vda row is disabled by the shipped disk identity rules.
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
"$vm" type /dev/vdb
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

# Done is a replacement surface and focuses Save backup. One Tab reaches Skip
# tour without opening the file picker.
"$vm" keys tab ret
sleep 8

# The default installed workspace focuses its shipped Terminal tile. Prove the
# configured hostname from that user-facing surface; no guest control channel
# or privileged observer is introduced for the test.
"$vm" type hostname
"$vm" keys ret
wait_screen 10-desktop-config 'cycle.?qos' 60 2 \
  || die "desktop terminal did not report the installed hostname"

copy_evidence target
printf 'PASS\n' > "$out/verdict.txt"
