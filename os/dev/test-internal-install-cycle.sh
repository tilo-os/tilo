#!/usr/bin/env bash
# USB-free installation round trip on one OVMF guest (design spec §9).
#
#   ./os/dev/test-internal-install-cycle.sh --fedora-sha256 SHA256 \
#       (--fedora-image PATH | --fedora-url URL) \
#       (--bundle-dir EXPORT_DIR | --installer-disk DISK) [--imager PATH] \
#       [--release-tag imager-vX.Y.Z] [--scenario NAME] [--graphics gl|software] \
#       [--out DIRECTORY]
#
# One virtual disk carries a real retained Fedora (Fedora Cloud Base, UEFI,
# pinned by SHA-256) followed by >=120 GiB of unallocated space. The retained
# system runs the real `imager --tilo-staging-request` (unprivileged) and
# `sudo imager --tilo-staging-helper`, reboots, and the firmware's one-shot
# BootNext starts the staged live installer from that same disk. The installer
# is driven only with QEMU keyboard/pointer events and observed with OCR, like
# os/dev/test-installer-cycle.sh. The installed Tilo then boots once (BootNext
# set by the installer), completes its first login, and reclaims the staging
# partitions; a final cold boot without BootNext must start the retained
# Fedora with its firmware order, ESP loaders and partitions intact.
#
# Scenarios (--scenario):
#   round-trip        (default) the complete flow above
#   interrupt-resume  kill -9 the helper during write-root, rerun the same
#                     request, require it to resume to a valid staged state
#   remove            stage, then `remove`: partitions, owned entry and
#                     BootNext gone, BootOrder unchanged
#   boot-next-busy    a foreign BootNext set before staging must be refused
#                     with `boot-next-busy` and nothing mutated
#
# Inputs:
#   --bundle-dir     output of os/dev/export-internal-bundle.sh (exported with
#                    RELEASE_ID == --release-tag); or
#   --installer-disk a built live installer disk (qcow2 or raw), exported here
#                    (needs `sudo -n`)
#   --imager         a test imager built by os/dev/build-internal-cycle-imager.sh
#                    for exactly this bundle; built here when omitted
#
# Evidence: every step writes machine-readable JSON under OUT/evidence/
# (partition table, firmware variables decoded from efivarfs bytes, ESP loader
# digests, machine-id, helper progress and outcomes, comparison verdicts) and
# appends to OUT/evidence/steps.jsonl. A missing prerequisite fails the run.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$dev_dir/../.." && pwd)"
vm="$dev_dir/vm.sh"
evidence_py="$dev_dir/internal_cycle_evidence.py"

die() { printf 'internal install cycle: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,43p' "$0" >&2; exit 2; }

fedora_image="" fedora_url="" fedora_sha256="" bundle_dir="" installer_disk="" imager=""
release_tag="${TILO_INTERNAL_CYCLE_RELEASE_TAG:-imager-v0.0.0}"
scenario=round-trip graphics=gl out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --fedora-image) fedora_image="${2:-}"; shift 2 ;;
    --fedora-url) fedora_url="${2:-}"; shift 2 ;;
    --fedora-sha256) fedora_sha256="${2:-}"; shift 2 ;;
    --bundle-dir) bundle_dir="${2:-}"; shift 2 ;;
    --installer-disk) installer_disk="${2:-}"; shift 2 ;;
    --imager) imager="${2:-}"; shift 2 ;;
    --release-tag) release_tag="${2:-}"; shift 2 ;;
    --scenario) scenario="${2:-}"; shift 2 ;;
    --graphics) graphics="${2:-}"; shift 2 ;;
    --out) out="${2:-}"; shift 2 ;;
    -h|--help) usage ;;
    *) printf 'unknown option: %s\n' "$1" >&2; usage ;;
  esac
done
case "$scenario" in round-trip|interrupt-resume|remove|boot-next-busy) ;; *) die "unknown scenario: $scenario" ;; esac
case "$graphics" in gl) display_arg=--gl ;; software) display_arg=--2d ;; *) die "graphics must be gl or software" ;; esac
[[ "$fedora_sha256" =~ ^[0-9a-f]{64}$ ]] || die "--fedora-sha256 must pin the Fedora Cloud Base image"
[ -n "$fedora_image$fedora_url" ] && [ -z "$fedora_image" -o -z "$fedora_url" ] \
  || die "pass exactly one of --fedora-image or --fedora-url"
[ -n "$bundle_dir$installer_disk" ] && [ -z "$bundle_dir" -o -z "$installer_disk" ] \
  || die "pass exactly one of --bundle-dir or --installer-disk"
[[ "$release_tag" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "--release-tag must be imager-vMAJOR.MINOR.PATCH"

export TILO_VM_ARCH="${TILO_VM_ARCH:-$(uname -m)}"
[ "$TILO_VM_ARCH" = x86_64 ] || die "the round trip uses the x86_64 Fedora Cloud Base image; run it on an x86_64 KVM host"

# --- prerequisites: fail, never skip -------------------------------------------
required=(qemu-img sfdisk mkfs.vfat mcopy ssh ssh-keygen scp python3 sha256sum truncate socat jq)
[ "$scenario" != round-trip ] || required+=(tesseract)
[ -z "$fedora_url" ] || required+=(curl)
for tool in "${required[@]}"; do
  command -v "$tool" >/dev/null 2>&1 || die "missing required tool: $tool"
done
if [ "${TILO_ACCEL:-auto}" != tcg ]; then
  [ -c /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ] \
    || die "/dev/kvm is not usable (set TILO_ACCEL=tcg only for a deliberately slow diagnostic run)"
fi

out="${out:-$PWD/internal-install-cycle-artifacts}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
export TILO_VM_CYCLE="${TILO_VM_CYCLE:-internal-install-cycle-$$}"
storage="${TILO_INTERNAL_CYCLE_STORAGE:-$(dirname "$out")/.$(basename "$out")-storage-$TILO_VM_CYCLE}"
run="${TILO_RUN:-${XDG_RUNTIME_DIR:-/tmp}/tilo-vm-$TILO_VM_CYCLE}"
cycle_dir="$storage/vm-cycles/$TILO_VM_CYCLE"
evidence="$out/evidence"
mkdir -p "$out" "$storage" "$evidence"
export TILO_INSTALLER_OUTPUT="$storage"
export TILO_RUN="$run"
export TILO_MEM="${TILO_MEM:-8192}"
export TILO_SSH_PORT="${TILO_SSH_PORT:-2222}"
export TILO_BOOT_WAIT_SECS="${TILO_BOOT_WAIT_SECS:-1800}"
# The disk passphrase and account password of the installed Tilo (a
# disposable test target; the same convention as test-installer-cycle.sh).
export TILO_USER="${TILO_INTERNAL_CYCLE_USER:-cycleuser}"
export TILO_PASS="${TILO_INTERNAL_CYCLE_PASSPHRASE:-cycle-passphrase-2026}"
free_gib="${TILO_INTERNAL_CYCLE_FREE_GIB:-128}"
root_growth_gib="${TILO_INTERNAL_CYCLE_ROOT_GROWTH_GIB:-40}"
target_bytes=$((64 << 30))
[ "$free_gib" -ge 120 ] || die "at least 120 GiB of unallocated space must follow the retained root"
retained="$cycle_dir/retained.qcow2"
# virtio disks expose no WWN; the installer and its backend require a stable
# hardware identity, so every boot gives the one disk the same serial.
disk_serial=tilo-retained-disk
guest_dir=/var/tmp/tilo-cycle

# shellcheck source=os/dev/installer-cycle-lib.sh
source "$dev_dir/installer-cycle-lib.sh"

# --- evidence ------------------------------------------------------------------
phase=setup
record_step() {
  python3 - "$evidence/steps.jsonl" "$1" "$2" "${3:-}" <<'PY'
import json, sys, time
path, step, status, artifact = sys.argv[1:]
with open(path, 'a', encoding='utf-8') as handle:
    handle.write(json.dumps({'step': step, 'status': status, 'artifact': artifact or None,
                             'at': int(time.time())}, sort_keys=True) + '\n')
PY
}
json_field() { python3 -c 'import json,sys; v=json.load(open(sys.argv[1]))
for k in sys.argv[2].split("."): v=v[k]
print(v)' "$1" "$2"; }

copy_serial() {
  local label="$1"
  if [ "$label" = installer ]; then
    # The live installer's stdout contains the one-time recovery key.
    printf 'withheld: live-installer serial may contain the one-time recovery key\n' \
      > "$out/installer-serial-withheld.txt"
  elif [ -f "$run/serial.log" ]; then
    tr -d '\000' < "$run/serial.log" > "$out/$label-serial.log"
  fi
  [ ! -f "$run/qemu.log" ] || cp -f "$run/qemu.log" "$out/$label-qemu.log"
}

cleanup() {
  local rc=$?
  trap - EXIT
  rm -f -- "$run/private-observe.png" "$run/private-observe.ppm"
  copy_serial "$phase" || true
  "$vm" stop >/dev/null 2>&1 || true
  printf '{"scenario":"%s","exit_status":%d,"phase":"%s"}\n' "$scenario" "$rc" "$phase" > "$evidence/verdict.json"
  scrub_artifact_secrets || rc=1
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  exit "$rc"
}
prove_secret_scrubber
trap cleanup EXIT

# --- guest access (per-run key; never in the artifact directory) ---------------
key="$storage/ssh/id_ed25519"
guest() {
  ssh -i "$key" -p "$TILO_SSH_PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o LogLevel=ERROR -o BatchMode=yes cycle@127.0.0.1 "$@"
}
guest_copy() {
  scp -q -i "$key" -P "$TILO_SSH_PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o LogLevel=ERROR -o BatchMode=yes "$1" "cycle@127.0.0.1:$2"
}
wait_ssh() {
  local attempts="${1:-180}" i
  for i in $(seq 1 "$attempts"); do
    guest true >/dev/null 2>&1 && return 0
    "$vm" status | grep -q '^running' || return 1
    sleep 5
  done
  return 1
}
wait_stopped() {
  local seconds="$1" i
  for i in $(seq 1 "$seconds"); do
    "$vm" status | grep -q '^stopped' && return 0
    sleep 1
  done
  return 1
}
collect() {
  local name="$1"
  guest "sudo python3 $guest_dir/internal_cycle_evidence.py collect" > "$evidence/$name.json" \
    || die "could not collect evidence $name"
  record_step "$name" collected "evidence/$name.json"
}
check() {
  local name="$1"; shift
  if python3 "$evidence_py" "$@" > "$evidence/$name.json"; then
    record_step "$name" pass "evidence/$name.json"
  else
    record_step "$name" fail "evidence/$name.json"
    cat "$evidence/$name.json" >&2
    die "evidence check $name failed"
  fi
}
helper_outcome() {
  python3 - "$1" <<'PY'
import json, sys
lines = [line for line in open(sys.argv[1], encoding='utf-8').read().splitlines() if line.strip()]
last = json.loads(lines[-1]) if lines else {}
print('ok' if last.get('ok') is True else 'refused:%s' % last.get('code') if 'ok' in last else 'none')
PY
}

# --- 1. inputs: bundle and a test imager that knows it ---------------------------
phase=inputs
"$vm" init-cycle
if [ -n "$installer_disk" ]; then
  [ -f "$installer_disk" ] || die "installer disk not found: $installer_disk"
  bundle_dir="$storage/bundle-export"
  rm -rf "$bundle_dir"
  "$dev_dir/export-internal-bundle.sh" "$installer_disk" "$TILO_VM_ARCH" "$release_tag" "$bundle_dir" >/dev/null
fi
manifest="$bundle_dir/bundle/tilo-internal-installer-$TILO_VM_ARCH.json"
esp_image="$bundle_dir/bundle/tilo-internal-installer-$TILO_VM_ARCH-esp.img"
root_image="$bundle_dir/bundle/tilo-internal-installer-$TILO_VM_ARCH-root.img"
for file in "$manifest" "$esp_image" "$root_image"; do
  [ -f "$file" ] && [ ! -L "$file" ] || die "bundle file missing: $file"
done
[ "$(jq -r .release_id "$manifest")" = "$release_tag" ] \
  || die "the bundle was exported as $(jq -r .release_id "$manifest"), not $release_tag"
manifest_sha256="$(sha256sum "$manifest" | awk '{print $1}')"
if [ -z "$imager" ]; then
  imager="$storage/imager"
  rm -f "$imager"
  "$dev_dir/build-internal-cycle-imager.sh" "$bundle_dir" "$release_tag" "$imager" >/dev/null
fi
[[ "$imager" = /* ]] && [ -f "$imager" ] && [ -x "$imager" ] || die "--imager must be an absolute executable"
grep -aFq "$manifest_sha256" "$imager" \
  || die "the imager's compiled-in catalog does not name this bundle (build it with os/dev/build-internal-cycle-imager.sh)"
jq -n --arg tag "$release_tag" --arg manifest "$manifest_sha256" \
  --arg imager "$(sha256sum "$imager" | awk '{print $1}')" --arg fedora "$fedora_sha256" \
  --arg scenario "$scenario" --argjson target "$target_bytes" --argjson free "$free_gib" \
  '{release_tag:$tag,manifest_sha256:$manifest,imager_sha256:$imager,fedora_image_sha256:$fedora,
    scenario:$scenario,target_bytes:$target,unallocated_gib:$free}' > "$evidence/01-inputs.json"
record_step 01-inputs ok evidence/01-inputs.json

# --- 2. retained Fedora disk --------------------------------------------------
phase=retained-setup
if [ -n "$fedora_url" ]; then
  fedora_image="$storage/fedora-cloud-base.qcow2"
  curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 -o "$fedora_image" "$fedora_url"
fi
[ -f "$fedora_image" ] || die "Fedora image not found: $fedora_image"
[ "$(sha256sum "$fedora_image" | awk '{print $1}')" = "$fedora_sha256" ] \
  || die "Fedora Cloud Base image does not match its pinned SHA-256"

# Grow a qcow2 by GIB and move the backup GPT to the new end, host-side on a
# sparse raw copy, so the guest never sees a misplaced alternate header.
grow_disk() {
  local image="$1" gib="$2" raw="$cycle_dir/grow.raw"
  qemu-img convert -f qcow2 -O raw "$image" "$raw"
  truncate -s "+${gib}G" "$raw"
  sfdisk --quiet --relocate gpt-bak-std "$raw"
  qemu-img convert -f raw -O qcow2 "$raw" "$image.new"
  mv -f "$image.new" "$image"
  rm -f -- "$raw"
}
qemu-img convert -f qcow2 -O qcow2 "$fedora_image" "$retained"
grow_disk "$retained" "$root_growth_gib"

mkdir -p "$storage/ssh" "$cycle_dir/seed"
chmod 0700 "$storage/ssh"
rm -f "$key" "$key.pub"
ssh-keygen -q -t ed25519 -N '' -C "tilo-internal-cycle-$TILO_VM_CYCLE" -f "$key"
# growpart/resize_rootfs are off: cloud-init would otherwise grow the root into
# the unallocated space on every boot. The root grows exactly once, by
# $root_growth_gib, room for the bundle; the free extent is added afterwards.
cat > "$cycle_dir/seed/user-data" <<EOF
#cloud-config
growpart: {mode: 'off'}
resize_rootfs: false
ssh_pwauth: false
users:
  - name: cycle
    shell: /bin/bash
    lock_passwd: true
    sudo: 'ALL=(ALL) NOPASSWD:ALL'
    ssh_authorized_keys: ['$(cat "$key.pub")']
runcmd:
  - [bash, -c, 'set -e; src=\$(findmnt -no SOURCE / | sed "s/\\[.*//"); disk=/dev/\$(lsblk -no PKNAME "\$src"); part=\$(cat /sys/class/block/\$(basename "\$src")/partition); growpart "\$disk" "\$part"; case \$(findmnt -no FSTYPE /) in btrfs) btrfs filesystem resize max / ;; ext4) resize2fs "\$src" ;; xfs) xfs_growfs / ;; esac; touch /var/lib/tilo-cycle-root-grown']
power_state: {mode: poweroff, timeout: 120, condition: true}
EOF
printf 'instance-id: tilo-cycle-%s\nlocal-hostname: retained-fedora\n' "$TILO_VM_CYCLE" > "$cycle_dir/seed/meta-data"
truncate -s 8M "$cycle_dir/seed.raw"
mkfs.vfat -n CIDATA "$cycle_dir/seed.raw" >/dev/null
mcopy -i "$cycle_dir/seed.raw" "$cycle_dir/seed/user-data" "$cycle_dir/seed/meta-data" ::/
qemu-img convert -f raw -O qcow2 "$cycle_dir/seed.raw" "$cycle_dir/seed.qcow2"
rm -f -- "$cycle_dir/seed.raw"

# First boot only carries the NoCloud seed; every later boot has exactly one
# disk, so firmware auto-created entries cannot differ between snapshots.
"$vm" boot --disk "$retained" --serial "$disk_serial" --attach "$cycle_dir/seed.qcow2" --2d --no-tpm
wait_stopped "${TILO_INTERNAL_CYCLE_FIRSTBOOT_SECONDS:-1200}" \
  || die "retained Fedora did not finish cloud-init and power off"
copy_serial fedora-firstboot
"$vm" stop >/dev/null 2>&1 || true
grow_disk "$retained" "$free_gib"
record_step 02-retained-disk ok

# --- 2b. baseline evidence -----------------------------------------------------
phase=retained
"$vm" boot --disk "$retained" --serial "$disk_serial" "$display_arg" --no-tpm
wait_ssh 180 || die "retained Fedora did not accept SSH"
guest "test -e /var/lib/tilo-cycle-root-grown" || die "cloud-init did not grow the retained root"
guest "command -v efibootmgr >/dev/null" || die "the retained Fedora lacks efibootmgr"
guest "install -d -m 0755 $guest_dir"
guest_copy "$evidence_py" "$guest_dir/internal_cycle_evidence.py"
collect 02-baseline

# --- 3. copy in the imager and bundle -----------------------------------------
need=$(( $(stat -c %s "$imager") + $(stat -c %s "$esp_image") + $(stat -c %s "$root_image") + (1 << 30) ))
avail="$(guest "df -B1 --output=avail $guest_dir | tail -n 1" | tr -d '[:space:]')"
[ "$avail" -ge "$need" ] || die "retained root has $avail bytes free, the bundle needs $need"
guest "install -d -m 0755 $guest_dir/bundle"
guest_copy "$imager" "$guest_dir/imager"
guest_copy "$manifest" "$guest_dir/bundle/"
guest_copy "$esp_image" "$guest_dir/bundle/"
guest_copy "$root_image" "$guest_dir/bundle/"
guest "cd $guest_dir && sha256sum imager bundle/*" > "$evidence/03-copied.sha256"
( cd "$(dirname "$imager")" && sha256sum "$(basename "$imager")" | awk '{print $1}' ) \
  | grep -Fxq "$(awk '$2 == "imager" {print $1}' "$evidence/03-copied.sha256")" \
  || die "imager changed in transit"
missing_libs="$(guest "ldd $guest_dir/imager | grep 'not found' || true")"
[ -z "$missing_libs" ] || die "the imager cannot run on the retained Fedora: $missing_libs"
jq -n --arg hostname cycle-tilo --arg username "$TILO_USER" \
  '{schema:1,hostname:$hostname,username:$username,full_name:"Cycle User",locale:"en_US.UTF-8",
    keyboard:"us",timezone:"UTC",filesystem:"btrfs"}' > "$storage/profile.json"
guest_copy "$storage/profile.json" "$guest_dir/profile.json"
record_step 03-copy-in ok evidence/03-copied.sha256

# --- 4. stage from the retained OS ------------------------------------------------
arch="$TILO_VM_ARCH"
request() {
  local name="$1" rc=0
  guest "cd $guest_dir && ./imager --tilo-staging-request \
    --manifest bundle/tilo-internal-installer-$arch.json \
    --component staging-esp=bundle/tilo-internal-installer-$arch-esp.img \
    --component staging-root=bundle/tilo-internal-installer-$arch-root.img \
    --target-bytes $target_bytes --profile-json profile.json --out $name.json" \
    > "$evidence/$name-summary.json" || rc=$?
  record_step "$name" "exit-$rc" "evidence/$name-summary.json"
  return "$rc"
}
run_helper() {
  local request_name="$1" progress="$2" rc=0
  guest "cd $guest_dir && sudo ./imager --tilo-staging-helper $request_name.json $progress.jsonl" \
    > "$evidence/$progress.stdout" 2> "$evidence/$progress.stderr" || rc=$?
  guest "cat $guest_dir/$progress.jsonl" > "$evidence/$progress.jsonl" || true
  record_step "$progress" "exit-$rc:$(helper_outcome "$evidence/$progress.jsonl")" "evidence/$progress.jsonl"
  return "$rc"
}
stage() {
  phase=stage
  request 04-request || die "staging request refused (see evidence/04-request-summary.json)"
  run_helper 04-request 04-progress || die "staging helper failed (see evidence/04-progress.jsonl)"
  [ "$(helper_outcome "$evidence/04-progress.jsonl")" = ok ] || die "staging helper did not report ok"
  collect 04-staged
  check 04-check-staged check-staged --baseline "$evidence/02-baseline.json" \
    --staged "$evidence/04-staged.json" --summary "$evidence/04-request-summary.json"
}

case "$scenario" in
  boot-next-busy)
    phase=boot-next-busy
    foreign="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["firmware"]["boot_order"][0])' "$evidence/02-baseline.json")"
    guest "sudo efibootmgr -q -n $foreign"
    collect 08-busy-before
    request 04-request || die "the request builder refused before the helper could decide (see evidence/04-request-summary.json)"
    if run_helper 04-request 04-progress; then die "staging succeeded despite a foreign BootNext"; fi
    [ "$(helper_outcome "$evidence/04-progress.jsonl")" = refused:boot-next-busy ] \
      || die "helper did not refuse with boot-next-busy"
    collect 08-busy-after
    check 08-check-busy check-unstaged --baseline "$evidence/08-busy-before.json" \
      --after "$evidence/08-busy-after.json" --expect-boot-next "$foreign"
    guest "sudo efibootmgr -q -N"
    phase=done
    printf 'PASS\n' > "$out/verdict.txt"
    exit 0
    ;;
  interrupt-resume)
    phase=interrupt
    request 04-request || die "staging request refused (see evidence/04-request-summary.json)"
    operation="$(json_field "$evidence/04-request-summary.json" operation_id)"
    guest "sudo systemd-run --quiet --collect --unit=tilo-cycle-helper -p Type=exec \
      -p WorkingDirectory=$guest_dir $guest_dir/imager --tilo-staging-helper \
      $guest_dir/04-request.json $guest_dir/04-progress-killed.jsonl"
    killed=0
    for _ in $(seq 1 1200); do
      if guest "cat $guest_dir/04-progress-killed.jsonl 2>/dev/null" | python3 -c '
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    item = json.loads(line)
    if "ok" in item:
        sys.exit(2)
    if item.get("step") == "write-root" and 0 < item.get("done_bytes", 0) < item.get("total_bytes", 0):
        sys.exit(0)
sys.exit(1)'; then
        guest "sudo systemctl kill --signal=KILL tilo-cycle-helper"
        killed=1
        break
      elif [ "$?" -eq 2 ]; then
        die "the helper finished before write-root could be interrupted"
      fi
      sleep 0.5
    done
    [ "$killed" -eq 1 ] || die "write-root never started"
    sleep 2
    guest "! systemctl is-active --quiet tilo-cycle-helper" || die "the killed helper is still running"
    guest "cat $guest_dir/04-progress-killed.jsonl" > "$evidence/04-progress-killed.jsonl"
    [ "$(helper_outcome "$evidence/04-progress-killed.jsonl")" = none ] || die "the killed helper wrote an outcome"
    guest "sudo cat /var/lib/tilo-imager/staging/$operation.json" > "$evidence/08-record-after-kill.json"
    python3 - "$evidence/08-record-after-kill.json" <<'PY' || die "the record does not show an interrupted write-root"
import json, sys
steps = json.load(open(sys.argv[1]))['steps']
write_root = [s['state'] for s in steps if s['step'] == 'write-root']
assert write_root and write_root[-1] == 'intended', write_root
PY
    record_step 08-killed ok evidence/08-record-after-kill.json
    run_helper 04-request 04-progress-resumed || die "the resumed helper failed"
    [ "$(helper_outcome "$evidence/04-progress-resumed.jsonl")" = ok ] || die "the resumed helper did not report ok"
    collect 04-staged
    check 08-check-resumed check-staged --baseline "$evidence/02-baseline.json" \
      --staged "$evidence/04-staged.json" --summary "$evidence/04-request-summary.json"
    phase=done
    printf 'PASS\n' > "$out/verdict.txt"
    exit 0
    ;;
  remove)
    stage
    phase=remove
    operation="$(json_field "$evidence/04-request-summary.json" operation_id)"
    guest "cd $guest_dir && ./imager --tilo-staging-request --action remove --record $operation --out 08-remove.json" \
      > "$evidence/08-remove-summary.json" || die "remove request refused"
    run_helper 08-remove 08-remove-progress || die "remove helper failed"
    [ "$(helper_outcome "$evidence/08-remove-progress.jsonl")" = ok ] || die "remove did not report ok"
    collect 08-removed
    check 08-check-removed check-unstaged --baseline "$evidence/02-baseline.json" --after "$evidence/08-removed.json"
    phase=done
    printf 'PASS\n' > "$out/verdict.txt"
    exit 0
    ;;
esac

stage
summary="$evidence/04-request-summary.json"
target_sectors="$(python3 -c 'import json,sys; t=json.load(open(sys.argv[1]))["target_extent"]; print(t["last_lba"]-t["first_lba"]+1)' "$summary")"
target_gib=$(( target_sectors * 512 >> 30 ))
root_gib=$(( (target_sectors * 512 - (2 << 30)) >> 30 ))

# --- 5. firmware BootNext -> staged live installer -------------------------------
copy_serial fedora-staging
phase=installer
guest "sudo systemctl reboot" >/dev/null 2>&1 || true
wait_screen 05-disk 'prepared[[:space:]]+space[[:space:]]+for[[:space:]]+Tilo[[:space:]]+next[[:space:]]+to[[:space:]]+Linux' 180 5 \
  || die "BootNext did not start the staged installer in internal mode"
ocr_flat "$out/05-disk.txt" | grep -Eqi 'setup[[:space:]]+choices[[:space:]]+are[[:space:]]+ready' \
  || die "the staged installer did not apply the handoff profile"
ocr_flat "$out/05-disk.txt" | grep -Eqi 'installed[[:space:]]+alongside[[:space:]]+Linux[[:space:]]+in[[:space:]]+the[[:space:]]+space[[:space:]]+Tilo[[:space:]]+Imager[[:space:]]+prepared' \
  || die "the Disk step did not preselect the staged plan"
record_step 05-staged-installer ok 05-disk.txt
click_text Next 05-click-next || die "could not find the Disk step's Next button"
wait_screen 06-account 'Create[[:space:]]+your[[:space:]]+account' 60 3 \
  || die "the staged disk was not accepted as the destination"
grep -Fqi -- "$TILO_USER" "$out/06-account.txt" || die "the handoff username did not reach the account screen"
"$vm" keys tab
"$vm" type "$TILO_PASS"
"$vm" keys tab
"$vm" type "$TILO_PASS"
"$vm" keys tab tab ret
wait_screen 07-review 'Review[[:space:]]+your[[:space:]]+choices' 60 3 || die "installer rejected the startup secret"
review="$(ocr_flat "$out/07-review.txt")"
grep -Eqi 'Also[[:space:]]+holds[[:space:]]+the[[:space:]]+running[[:space:]]+installer' <<<"$review" \
  || die "Review does not state that the disk also holds the running installer"
grep -Eqi 'Linux[[:space:]]+stays[[:space:]]+untouched' <<<"$review" \
  || die "Review does not state that the existing system stays untouched"
grep -Eqi "${root_gib}[[:space:]]+GiB[[:space:]]+Tilo[[:space:]]+root[[:space:]]+in[[:space:]]+${target_gib}[[:space:]]+GiB" <<<"$review" \
  || die "Review's partition plan is not the staged ${target_gib} GiB extent"
grep -Eqi 'cycle.?tilo' <<<"$review" || die "Review did not show the handoff hostname"
jq -n --argjson target_gib "$target_gib" --argjson root_gib "$root_gib" \
  '{review_running_installer_line:true,review_existing_system_untouched:true,plan_target_gib:$target_gib,plan_root_gib:$root_gib}' \
  > "$evidence/07-review.json"
record_step 07-review pass evidence/07-review.json
# Guided alongside confirms with the whole-disk name; the staged live system
# runs from the single virtual disk.
"$vm" type /dev/vda
"$vm" keys tab spc tab tab ret
wait_private_screen 'Tilo[[:space:]]+is[[:space:]]+installed|Installation[[:space:]]+complete' \
  "$(( ${TILO_INTERNAL_CYCLE_MAX_INSTALL_SECONDS:-3600} / 5 ))" 5 \
  || die "installer did not reach its Finish screen"
# Copy key -> acknowledge -> Power off (the same route as test-installer-cycle.sh).
"$vm" keys ret tab spc tab tab ret
wait_stopped 120 || die "graphical Finish action did not power off"
copy_serial installer
"$vm" stop >/dev/null 2>&1 || true
record_step 07-installed ok

# --- 6. BootNext -> installed Tilo, first login, reclaim -------------------------
phase=target
"$vm" boot --disk "$retained" --serial "$disk_serial" "$display_arg" --no-tpm
unlock_target_to_greeter "$TILO_PASS" 09-target-greeter \
  || die "the installer's BootNext did not start the installed Tilo (or it did not unlock)"
record_step 09-target-boot ok 09-target-greeter.txt
complete_first_login "$TILO_PASS" 10
reclaim=""
for _ in $(seq 1 "$(( ${TILO_INTERNAL_CYCLE_RECLAIM_SECONDS:-2400} / 10 ))"); do
  reclaim="$(tr -d '\000' < "$run/serial.log" | sed 's/\r//g' \
    | grep -aoE 'tilo-staging-reclaim: (done|refused: .*)' | tail -n 1 || true)"
  [ -z "$reclaim" ] || break
  "$vm" status | grep -q '^running' || die "installed Tilo stopped before reclaiming"
  sleep 10
done
jq -n --arg marker "$reclaim" '{marker:$marker}' > "$evidence/11-reclaim.json"
case "$reclaim" in
  'tilo-staging-reclaim: done') record_step 11-reclaim pass evidence/11-reclaim.json ;;
  '') die "tilo-staging-reclaim never reported done or refused" ;;
  *) record_step 11-reclaim fail evidence/11-reclaim.json; die "reclaim refused: $reclaim" ;;
esac
"$vm" cmd system_powerdown >/dev/null
if ! wait_stopped 180; then
  record_step 11-poweroff forced
  "$vm" stop >/dev/null 2>&1 || true
fi
copy_serial target
"$vm" stop >/dev/null 2>&1 || true

# --- 7. cold boot without BootNext -> retained Fedora ---------------------------
phase=retained-final
"$vm" boot --disk "$retained" --serial "$disk_serial" --2d --no-tpm
wait_ssh 180 || die "the retained Fedora did not boot after the round trip"
collect 12-final
check 12-check-final check-final --baseline "$evidence/02-baseline.json" \
  --staged "$evidence/04-staged.json" --summary "$summary" --final "$evidence/12-final.json"
guest "sudo systemctl poweroff" >/dev/null 2>&1 || true
wait_stopped 120 || true
phase=done
printf 'PASS\n' > "$out/verdict.txt"
