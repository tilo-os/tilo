#!/usr/bin/env bash
# The server plan install cycle: a Linux machine planned as a Tilo Server by
# another of the person's devices installs it unattended, from a staged
# installer handed over with the machine's plan and single-use token, and then
# opens its encrypted disk with the consent of the owner's device — on its
# first boot and again after a restart — with nobody at its screen and no TPM.
#
#   ./os/dev/test-server-plan-install-cycle.sh \
#       (--bundle-dir EXPORT_DIR | --installer-disk DISK) --imager PATH \
#       --owner-disk DEV_DISK \
#       (--fedora-image PATH | --fedora-url URL) --fedora-sha256 SHA256 \
#       [--release-tag imager-vX.Y.Z] [--out DIRECTORY] \
#       [--development-layers DIRECTORY] [--owner-layers DIRECTORY]
#
# One OVMF guest boots a real retained Fedora (Fedora Cloud Base, UEFI,
# pinned by SHA-256) with a second, empty disk. The staging helper starts in a
# privileged container on that guest — the way a machine with no bootable
# medium stages from its live system — and names the second disk by its
# serial, never by a path. The staged live installer finds the machine's plan
# and ticket in its handoff and installs Tilo Server with nobody at the screen.
#
# A second guest is the owner's device: the dev disk's session, started
# headless with the throwaway identity a development profile mints. It plans
# the machine (`tilo machine add`), serves the ticket the machine joins with,
# approves its unlock keyslot, holds the key the install hands over, lets the first boot and the restart after
# `tilo server set-default-start` unlock its disk, and reads the recovery key
# back with `tilo server recovery-key`.
#
# With --development-layers, the staging request also carries a directory of
# component layers (`tilo-vm layer-pack --incoming`): they ride beside the plan
# and the token into the staged root, and the installed development server
# applies them at its first boot, before its services start. With
# --owner-layers, the owner's device applies such a directory the same way
# (`tilo-vm --local apply-incoming`) before its session starts.
#
# Evidence: every stage is recorded under OUT/evidence; serials that may hold
# a secret are never copied. As the run ends, a failure included, each guest's
# serial and QEMU logs are copied to OUT/<guest>-serial.log and
# OUT/<guest>-qemu.log, the owner's profile logs to OUT/owner-logs.tar, and
# OUT/evidence/verdict.json names the phase it ended in. Nothing a guest
# wrote is deleted, so a failure can be read again afterwards.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$dev_dir/../.." && pwd)"
vm="$dev_dir/vm.sh"
# shellcheck source=os/dev/action-frame.sh
source "$dev_dir/action-frame.sh"

die() { printf 'server plan install cycle: %s\n' "$*" >&2; exit 1; }
note() { printf 'server plan install cycle: %s\n' "$*" >&2; }
usage() { sed -n '2,35p' "$0" >&2; exit 2; }

bundle_dir="" installer_disk="" imager="" owner_disk="" development_layers="" owner_layers=""
fedora_image="" fedora_url="" fedora_sha256="" out=""
release_tag="${TILO_INTERNAL_CYCLE_RELEASE_TAG:-imager-v0.0.0}"
# The same Fedora Cloud Base the Linux VM end-to-end check pins.
default_fedora_url="https://download.fedoraproject.org/pub/fedora/linux/releases/44/Cloud/x86_64/images/Fedora-Cloud-Base-Generic-44-1.7.x86_64.qcow2"
default_fedora_sha256="28680fe5b371a5a82ebf43a31926e086a168e59949d03969c5093e7071f90b7f"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bundle-dir) bundle_dir="${2:-}"; shift 2 ;;
    --installer-disk) installer_disk="${2:-}"; shift 2 ;;
    --imager) imager="${2:-}"; shift 2 ;;
    --owner-disk) owner_disk="${2:-}"; shift 2 ;;
    --fedora-image) fedora_image="${2:-}"; shift 2 ;;
    --fedora-url) fedora_url="${2:-}"; shift 2 ;;
    --fedora-sha256) fedora_sha256="${2:-}"; shift 2 ;;
    --release-tag) release_tag="${2:-}"; shift 2 ;;
    --development-layers) development_layers="${2:-}"; shift 2 ;;
    --owner-layers) owner_layers="${2:-}"; shift 2 ;;
    --out) out="${2:-}"; shift 2 ;;
    -h|--help) usage ;;
    *) printf 'unknown option: %s\n' "$1" >&2; usage ;;
  esac
done

[ -n "$bundle_dir$installer_disk" ] && [ -z "$bundle_dir" -o -z "$installer_disk" ] \
  || die 'pass exactly one of --bundle-dir or --installer-disk'
[ -f "$owner_disk" ] || die "--owner-disk must name the dev disk: $owner_disk"
[[ "$release_tag" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die '--release-tag must be imager-vMAJOR.MINOR.PATCH'
[ -n "$fedora_sha256" ] || fedora_sha256="$default_fedora_sha256"
[[ "$fedora_sha256" =~ ^[0-9a-f]{64}$ ]] || die '--fedora-sha256 must pin the Fedora Cloud Base image'
[ -n "$fedora_image" ] && [ -n "$fedora_url" ] \
  && die 'pass at most one of --fedora-image or --fedora-url'
[ -n "$fedora_image" ] || fedora_url="${fedora_url:-$default_fedora_url}"

export TILO_VM_ARCH="${TILO_VM_ARCH:-$(uname -m)}"
[ "$TILO_VM_ARCH" = x86_64 ] || die 'the server plan cycle uses the x86_64 Fedora Cloud Base image'

required=(qemu-img sfdisk mkfs.vfat mcopy ssh ssh-keygen tar python3 sha256sum truncate jq curl)
for tool in "${required[@]}"; do
  command -v "$tool" >/dev/null 2>&1 || die "missing required tool: $tool"
done
[ "${TILO_ACCEL:-auto}" = tcg ] || { [ -c /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; } \
  || die '/dev/kvm is not usable (set TILO_ACCEL=tcg only for a deliberately slow diagnostic pass)'

out="${out:-$PWD/server-plan-install-cycle-artifacts}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
evidence="$out/evidence"
mkdir -p "$evidence"
export TILO_VM_CYCLE="${TILO_VM_CYCLE:-server-plan-cycle-$$}"
# Each VM's run directory holds Unix sockets, whose paths a kernel bounds at
# 108 bytes, so keep this beside the artifacts under a short name.
storage="${TILO_SERVER_CYCLE_STORAGE:-$(dirname "$out")/.plan-cycle-$TILO_VM_CYCLE}"
mkdir -p "$storage"
cycle_dir="$storage"
mkdir -p "$cycle_dir"

export TILO_BOOT_WAIT_SECS="${TILO_BOOT_WAIT_SECS:-1800}"
export TILO_USER="${TILO_USER:-tester}"
export TILO_PASS="${TILO_PASS:-tilo}"
server_dir="$cycle_dir/server-vm"
owner_dir="$cycle_dir/owner-vm"
mkdir -p "$server_dir" "$owner_dir"
server_port="${TILO_SERVER_CYCLE_SSH_PORT:-22240}"
owner_port="${TILO_SERVER_CYCLE_OWNER_SSH_PORT:-22241}"
owner_root=/var/tmp/tilo-server-plan-owner
owner_driver=$owner_root/driver.sh
server_name=cycle-server

# The guest VM, through this toolkit's SSH: the retained Fedora's own key.
server_ssh_key="$storage/ssh/id_ed25519"
server() {
  ssh -i "$server_ssh_key" -p "$server_port" -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o LogLevel=ERROR \
    -o BatchMode=yes cycle@127.0.0.1 "$@"
}
# The machine under installation, booted from the retained Fedora.
# The machine and the owner device share this check's own VM, whose kernel
# has a little under 8 GiB: a guest of 8 GiB beside the owner is refused its
# memory outright, and QEMU stops before the firmware prints a line.
server_vm() {
  TILO_RUN="$server_dir" TILO_SSH_PORT="$server_port" \
    TILO_MEM="${TILO_SERVER_CYCLE_SERVER_MEM:-4096}" "$vm" "$@"
}
# The owner device, booted from the dev disk.
owner() {
  TILO_RUN="$owner_dir" TILO_SSH_PORT="$owner_port" \
    TILO_MEM="${TILO_SERVER_CYCLE_OWNER_MEM:-4096}" "$vm" "$@"
}
# A command in the owner's private profile on that guest.
# Every guest command is bounded: a guest that accepts the connection and
# never answers would otherwise hold the cycle until its own timeout.
owner_profile() {
  TILO_SSH_BOUND="${TILO_SSH_BOUND:-660}" owner ssh "bash $owner_driver $owner_root $*"
}
owner_control() {
  TILO_SSH_BOUND="${TILO_SSH_BOUND:-120}" owner ssh "bash $owner_driver $owner_root control $(printf '%q ' "$@")"
}
owner_cmd() {
  TILO_SSH_BOUND="${TILO_SSH_BOUND:-600}" owner ssh "bash $owner_driver $owner_root cmd $(printf '%q ' "$@")"
}

wait_server_ssh() {
  local attempts="${1:-180}" i
  for i in $(seq 1 "$attempts"); do
    TILO_SSH_BOUND=10 server true >/dev/null 2>&1 && return 0
    server_vm status | grep -q '^running' || return 1
    sleep 5
  done
  return 1
}
wait_owner_ssh() {
  local attempts="${1:-180}" i
  for i in $(seq 1 "$attempts"); do
    TILO_SSH_BOUND=10 owner ssh true >/dev/null 2>&1 && return 0
    owner status 2>/dev/null | grep -q '^running' || return 1
    sleep 2
  done
  return 1
}

# What a guest said on its serial line, where a boot that never answered
# explains itself.
serial_tail() {
  local label=$1 vm_dir=$2
  [ ! -f "$vm_dir/serial.log" ] || {
    printf -- '--- %s serial (last 60 lines):\n' "$label" >&2
    tr -d '\000' < "$vm_dir/serial.log" | tail -n 60 >&2 || true
  }
}

phase=setup
copy_logs() {
  local label=$1 vm_dir=$2
  [ ! -f "$vm_dir/serial.log" ] || tr -d '\000' < "$vm_dir/serial.log" > "$out/$label-serial.log"
  [ ! -f "$vm_dir/qemu.log" ] || cp -f "$vm_dir/qemu.log" "$out/$label-qemu.log"
}
copy_owner_profile() {
  # The profile's logs only: its identity holds the person's private keys.
  owner ssh "cd $owner_root && tar -cf - logs 2>/dev/null" \
    > "$out/owner-logs.tar" 2>/dev/null || true
  owner_control questions --json > "$out/owner-questions.json" 2>&1 || true
}
cleanup() {
  local rc=$?
  trap - EXIT
  # A person-driven install shows its one-time recovery key on the serial
  # (os/dev/test-internal-install-cycle.sh withholds that serial); a planned
  # install hands the key to the owner's devices and prints none. Its serial
  # is what says why an install never asked the owner anything, so it is
  # kept, without a line that names a recovery key all the same.
  copy_logs server "$server_dir" || true
  if [ -f "$out/server-serial.log" ]; then
    grep -viE 'recovery[ _-]?key' "$out/server-serial.log" > "$out/server-serial.kept" || true
    mv -f "$out/server-serial.kept" "$out/server-serial.log"
  fi
  copy_logs owner "$owner_dir" || true
  TILO_SSH_BOUND=30 copy_owner_profile || true
  TILO_SSH_BOUND=30 owner_profile stop >/dev/null 2>&1 || true
  owner stop >/dev/null 2>&1 || true
  server_vm stop >/dev/null 2>&1 || true
  printf '{"exit_status":%d,"phase":"%s"}\n' "$rc" "$phase" > "$evidence/verdict.json"
  exit "$rc"
}
trap cleanup EXIT

mark_step() {
  python3 - "$evidence/steps.jsonl" "$1" "$2" <<'PY'
import json, sys, time
path, step, status = sys.argv[1:]
with open(path, 'a', encoding='utf-8') as handle:
    handle.write(json.dumps({'step': step, 'status': status,
                             'at': int(time.time())}, sort_keys=True) + '\n')
PY
}

# --- inputs: the server installer and the test imager that names it ----------
phase=inputs
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
[[ "$imager" = /* ]] && [ -f "$imager" ] && [ -x "$imager" ] || die '--imager must be an absolute executable'
grep -aFq "$manifest_sha256" "$imager" \
  || die "the imager's compiled-in catalog does not name this bundle"
jq -n --arg tag "$release_tag" --arg manifest "$manifest_sha256" \
  --arg imager "$(sha256sum "$imager" | awk '{print $1}')" --arg fedora "$fedora_sha256" \
  '{release_tag:$tag,manifest_sha256:$manifest,imager_sha256:$imager,fedora_image_sha256:$fedora}' \
  > "$evidence/01-inputs.json"
mark_step 01-inputs ok

# --- the owner device: a development profile with a throwaway identity -------
phase=owner
qemu-img create -f qcow2 -F qcow2 -b "$(cd "$(dirname "$owner_disk")" && pwd)/$(basename "$owner_disk")" \
  "$owner_dir/disk.qcow2" >/dev/null
owner boot --disk "$owner_dir/disk.qcow2" --2d --no-tpm || die 'the owner device did not start'
wait_owner_ssh 120 || {
  owner status >&2 2>&1 || true
  serial_tail owner "$owner_dir"
  die 'the owner device did not accept SSH'
}
owner ssh 'mkdir -p /var/tmp' >/dev/null
owner ssh "mkdir -p $owner_root" >/dev/null
owner ssh "cat > $owner_driver" < "$dev_dir/server-plan-owner.sh" \
  || die 'the owner driver did not reach the guest'
if [ -n "$owner_layers" ]; then
  # The components this run rebuilt, applied as a developer swaps them into a
  # device: imported and merged through the device's own tilo-vmd.
  [ -d "$owner_layers" ] || die "--owner-layers must name a directory: $owner_layers"
  owner ssh "printf '%s\\n' '$TILO_PASS' | sudo -S -p '' install -d -m 0700 /var/lib/tilo-vm/incoming" \
    || die 'the owner device has no tilo-vm state to take layers into'
  for layer in "$owner_layers"/*; do
    [ -f "$layer" ] && [ ! -L "$layer" ] || die "--owner-layers holds something not a regular file: $layer"
    owner ssh "cat > /var/tmp/$(basename "$layer")" < "$layer" \
      || die "the owner device did not take $(basename "$layer")"
    owner ssh "printf '%s\\n' '$TILO_PASS' | sudo -S -p '' install -m 0600 /var/tmp/$(basename "$layer") /var/lib/tilo-vm/incoming/ && rm -f /var/tmp/$(basename "$layer")" \
      || die "the owner device did not stage $(basename "$layer")"
  done
  TILO_SSH_BOUND=600 owner ssh "printf '%s\\n' '$TILO_PASS' | sudo -S -p '' tilo-vm --local apply-incoming" \
    > "$evidence/02-owner-layers.txt" 2>&1 \
    || { cat "$evidence/02-owner-layers.txt" >&2; die 'the owner device did not apply its layers'; }
  cat "$evidence/02-owner-layers.txt" >&2
fi
note 'the owner device is up'
owner_profile start || die 'the owner profile did not start'
owner_profile wait || die 'the owner session did not answer'
note 'the owner session answers'
mark_step 02-owner ok

# The person plans the machine: the plan goes into their Servers workspace,
# and the single-use token stays on this device, in a file only it reads.
# The session's workspace service comes up after its control socket, so ask
# the same actions a cheap question first and give it a bound to answer in.
for attempt in $(seq 1 12); do
  TILO_SSH_BOUND=30 owner_cmd tilo machine list >/dev/null 2>&1 && break
  sleep 5
done
owner_cmd tilo machine add --os server --name "$server_name" \
  --disk serial:tilo-install-target --network dhcp --unlock owner-device \
  --updates stable --no-workspaces --token-file "$owner_root/machine.token" \
  > "$evidence/03-plan.txt" 2>&1 \
  || {
    cat "$evidence/03-plan.txt" >&2
    TILO_SSH_BOUND=30 owner ssh "tail -n 120 $owner_root/logs/session.log" >&2 2>&1 || true
    die 'the machine could not be planned'
  }
plan_id="$(sed -n 's/^plan \([0-9a-f]\{32\}\).*/\1/p' "$evidence/03-plan.txt" | head -n 1)"
[[ "$plan_id" =~ ^[0-9a-f]{32}$ ]] || die "the plan's id is not in the command's answer: $(cat "$evidence/03-plan.txt")"
{ owner_control raw "$(action_frame tilo.machines machines machine.show "{\"plan\":\"$plan_id\"}")" \
    > "$evidence/03-plan-entry.json" \
    && action_completed "$evidence/03-plan-entry.json"; } \
  || { cat "$evidence/03-plan-entry.json" >&2; die 'the plan entry could not be read back'; }
jq -e '.message.Receipt.status.Completed.output.value.plan as $plan
  | $plan.plan == $id and ($plan.open | length == 0)' --arg id "$plan_id" \
  "$evidence/03-plan-entry.json" >/dev/null \
  || die 'the plan entry is not the complete plan the machine installs from'
jq '.message.Receipt.status.Completed.output.value.plan' "$evidence/03-plan-entry.json" > "$storage/plan.json"
owner ssh "cat $owner_root/machine.token" > "$storage/machine.token" \
  || die 'the token could not be taken from the owner device'
chmod 0600 "$storage/machine.token" "$storage/plan.json"
note "the machine is planned: $plan_id"
mark_step 03-plan ok

# --- the machine: retained Fedora with an empty second disk ------------------
phase=retained-setup
if [ -n "$fedora_url" ] && [ -z "$fedora_image" ]; then
  fedora_image="$storage/fedora-cloud-base.qcow2"
  curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 -o "$fedora_image" "$fedora_url"
fi
[ -f "$fedora_image" ] || die "Fedora image not found: $fedora_image"
[ "$(sha256sum "$fedora_image" | awk '{print $1}')" = "$fedora_sha256" ] \
  || die 'Fedora Cloud Base image does not match its pinned SHA-256'

# The base is grown with an overlay of a larger virtual size, and the guest
# moves its own backup GPT header and grows its root at first boot: no
# gigabyte raw copy is written by the test machine.
retained="$cycle_dir/retained.qcow2"
base_gib="$(qemu-img info --output=json "$fedora_image" | jq -r '."virtual-size"' | awk '{printf "%d", ($1 + 1073741823) / 1073741824}')"
root_growth_gib="${TILO_SERVER_CYCLE_ROOT_GROWTH_GIB:-50}"
qemu-img create -f qcow2 -F qcow2 -b "$(cd "$(dirname "$fedora_image")" && pwd)/$(basename "$fedora_image")" \
  "$retained" "$((base_gib + root_growth_gib))G" >/dev/null
target_disk="$cycle_dir/target.qcow2"
target_gib="${TILO_SERVER_CYCLE_TARGET_GIB:-128}"
qemu-img create -f qcow2 "$target_disk" "${target_gib}G" >/dev/null
# The installed system gets what the staging partitions the bundle asks for
# leave of the disk, less a GiB for the partition table and alignment.
staging_bytes="$(jq '[.components[].minimum_partition_bytes] | add' "$manifest")"
target_bytes=$(( (target_gib << 30) - staging_bytes - (1 << 30) ))
[ "$target_bytes" -ge $((16 << 30)) ] \
  || die "a ${target_gib} GiB disk leaves $target_bytes bytes beside the ${staging_bytes}-byte staging"

mkdir -p "$storage/ssh"
chmod 0700 "$storage/ssh"
rm -f "$server_ssh_key" "$server_ssh_key.pub"
ssh-keygen -q -t ed25519 -N '' -C "tilo-server-plan-$TILO_VM_CYCLE" -f "$server_ssh_key"
seed="$cycle_dir/seed"
mkdir -p "$seed"
cat > "$seed/user-data" <<EOF
#cloud-config
growpart: {mode: 'off'}
resize_rootfs: false
ssh_pwauth: false
users:
  - name: cycle
    shell: /bin/bash
    lock_passwd: true
    sudo: 'ALL=(ALL) NOPASSWD:ALL'
    ssh_authorized_keys: ['$(cat "$server_ssh_key.pub")']
runcmd:
  - [bash, -c, 'set -e; src=\$(findmnt -no SOURCE / | sed "s/\\[.*//"); disk=/dev/\$(lsblk -no PKNAME "\$src"); sfdisk --relocate gpt-bak-std --no-reread --no-tell-kernel "\$disk" || true; part=\$(cat /sys/class/block/\$(basename "\$src")/partition); growpart "\$disk" "\$part"; case \$(findmnt -no FSTYPE /) in btrfs) btrfs filesystem resize max / ;; ext4) resize2fs "\$src" ;; xfs) xfs_growfs / ;; esac; touch /var/lib/tilo-cycle-root-grown']
power_state: {mode: poweroff, timeout: 120, condition: true}
EOF
printf 'instance-id: tilo-server-plan-%s\nlocal-hostname: retained-fedora\n' "$TILO_VM_CYCLE" > "$seed/meta-data"
truncate -s 8M "$cycle_dir/seed.raw"
mkfs.vfat -n CIDATA "$cycle_dir/seed.raw" >/dev/null
mcopy -i "$cycle_dir/seed.raw" "$seed/user-data" "$seed/meta-data" ::/
qemu-img convert -f raw -O qcow2 "$cycle_dir/seed.raw" "$cycle_dir/seed.qcow2"
rm -f -- "$cycle_dir/seed.raw"

server_vm boot \
  --disk "$retained" --serial tilo-retained-disk --attach "$target_disk" \
  --attach "$cycle_dir/seed.qcow2" --2d --no-tpm \
  || die 'the retained Fedora did not start'
for i in $(seq 1 1200); do
  server_vm status | grep -q '^stopped' && break
  [ "$i" -lt 1200 ] || die 'the retained Fedora did not finish cloud-init and power off'
  sleep 1
done
server_vm stop >/dev/null 2>&1 || true

# What the person would have downloaded onto the machine: the Imager, the
# bundle, the plan and its token, as one tar the machine reads off a disk of
# its own. Over the user-mode network, scp moved them at a few MB/s.
transfer="$cycle_dir/transfer"
rm -rf "$transfer"
mkdir -p "$transfer/bundle"
ln -s "$(realpath "$imager")" "$transfer/imager"
for file in "$manifest" "$esp_image" "$root_image"; do
  ln -s "$(realpath "$file")" "$transfer/bundle/$(basename "$file")"
done
ln -s "$storage/plan.json" "$transfer/plan.json"
ln -s "$storage/machine.token" "$transfer/machine.token"
if [ -n "$development_layers" ]; then
  [ -d "$development_layers" ] \
    || die "--development-layers must name a directory: $development_layers"
  mkdir -p "$transfer/development-layers"
  for layer in "$development_layers"/*; do
    [ -f "$layer" ] && [ ! -L "$layer" ] || continue
    ln -s "$(realpath "$layer")" "$transfer/development-layers/$(basename "$layer")"
  done
  layers_copied=$(ls -A "$transfer/development-layers" | wc -l)
  [ "$layers_copied" -gt 0 ] \
    || die "--development-layers holds no regular files: $development_layers"
fi
tar -C "$transfer" --dereference --sparse --owner=0 --group=0 -cf "$cycle_dir/transfer.tar" .
qemu-img convert -f raw -O qcow2 "$cycle_dir/transfer.tar" "$cycle_dir/transfer.qcow2"
rm -rf -- "$transfer" "$cycle_dir/transfer.tar"

server_vm boot \
  --disk "$retained" --serial tilo-retained-disk --attach "$target_disk" \
  --attach "$cycle_dir/transfer.qcow2" --2d --no-tpm \
  || die 'the retained Fedora did not start'
wait_server_ssh 240 || {
  server_vm status >&2 2>&1 || true
  serial_tail server "$server_dir"
  die 'the retained Fedora did not accept SSH'
}
server 'test -e /var/lib/tilo-cycle-root-grown' || die 'cloud-init did not grow the retained root'
# podman runs the privileged staging container; efibootmgr reads back the
# boot entry it writes.
server 'command -v podman >/dev/null && command -v efibootmgr >/dev/null' \
  || server 'sudo dnf install -y --setopt=install_weak_deps=False podman util-linux efibootmgr >/dev/null' \
  || die 'the retained Fedora has no podman and efibootmgr for the staging'
# A disk nobody has partitioned yet carries no table at all, which `sfdisk
# --json` answers as an error rather than an empty list: `lsblk` reads the
# device either way.
server "sudo lsblk -J -b -o NAME,TYPE,PARTN /dev/vdb" > "$evidence/04-target-empty.json" \
  || die 'the target disk cannot be read'
python3 - "$evidence/04-target-empty.json" <<'PY' || die 'the target disk is not empty'
import json, sys
rows = json.load(open(sys.argv[1]))['blockdevices']
assert len(rows) == 1 and rows[0]['type'] == 'disk', rows
assert not rows[0].get('children'), rows
PY
# Staging writes where a disk's partition table says nothing is used yet, so a
# disk the person has not prepared takes the empty GPT their other disks carry
# before the request names it.
printf 'label: gpt\n' | server "sudo sfdisk /dev/vdb" \
  > "$evidence/04-target-labelled.txt" 2>&1 \
  || die 'the target disk could not take a GPT'
note 'the retained Fedora is up and its target disk is empty'
mark_step 04-retained ok

# --- copy the imager, bundle, plan and token into the machine ----------------
phase=copy-in
need=$(( $(stat -c %s "$cycle_dir/transfer.qcow2") + (1 << 30) ))
avail="$(server "df -B1 --output=avail /var/tmp | tail -n 1" | tr -d ' \t\n')"
[ "$avail" -ge "$need" ] || die "the retained root has $avail bytes free, the bundle needs $need"
guest_dir=/var/tmp/tilo-server-plan
# The unpack runs as a unit of the machine's own, not under the SSH session:
# a session that ended part way through ten gigabytes (the connection closed
# by the machine) took the unpack with it.
server "install -d -m 0755 $guest_dir && sudo systemd-run --quiet --unit=tilo-cycle-copy-in \
  -p Type=oneshot -p RemainAfterExit=yes \
  bash -c 'tar -C $guest_dir --no-same-owner -xSf /dev/disk/by-id/virtio-tilo-attach-1 && chown -R cycle:cycle $guest_dir'" \
  || die 'the machine did not start unpacking the installer and its bundle'
unpacked=""
for i in $(seq 1 1800); do
  state="$(server 'systemctl is-active tilo-cycle-copy-in' 2>/dev/null || true)"
  case "$state" in
    active) unpacked=1; break ;;
    failed) break ;;
  esac
  sleep 2
done
server 'journalctl -b -u tilo-cycle-copy-in --no-pager' > "$evidence/05-copy-in.txt" 2>&1 || true
[ -n "$unpacked" ] || { tail -n 20 "$evidence/05-copy-in.txt" >&2; die "the machine could not unpack the installer and its bundle (${state:-no answer})"; }
server "chmod 0600 $guest_dir/machine.token"
layers_flag=""
if [ -n "$development_layers" ]; then
  server "chmod 0700 $guest_dir/development-layers && chmod 0600 $guest_dir/development-layers/*"
  layers_flag="--development-layers development-layers"
  note "the machine carries $layers_copied prebuilt development layer(s)"
fi
missing_libs="$(server "ldd $guest_dir/imager | grep 'not found' || true")"
[ -z "$missing_libs" ] || die "the imager cannot start on the retained Fedora: $missing_libs"
note 'the installer, its bundle, the plan and its token are on the machine'
mark_step 05-copy-in ok

# --- stage onto the second disk from a privileged container ------------------
# The request names the disk by its serial and carries the machine's plan and
# token; the helper resolves and re-probes that disk inside the container,
# where the mount table names no system disk at all.
phase=stage
server "cd $guest_dir && ./imager --tilo-staging-request --action stage \
  --disk serial:tilo-install-target \
  --manifest bundle/tilo-internal-installer-$TILO_VM_ARCH.json \
  --component staging-esp=bundle/tilo-internal-installer-$TILO_VM_ARCH-esp.img \
  --component staging-root=bundle/tilo-internal-installer-$TILO_VM_ARCH-root.img \
  --target-bytes $target_bytes \
  --plan plan.json --token-file machine.token \
  $layers_flag \
  --out 06-request.json" > "$evidence/06-request-summary.json" 2>&1 \
  || { cat "$evidence/06-request-summary.json" >&2; die 'the staging request was refused'; }
server "sudo install -d -m 0700 /var/lib/tilo-imager"
# The request names the bundle, the plan and the token by the absolute paths
# the guest knows, so the container mounts that tree at the same path. The
# helper trusts files of root or of the person sudo ran it for (SUDO_UID), as
# `sudo imager --tilo-staging-helper` does; sudo's variable is handed into the
# container, which starts with an environment of its own. The helper reads and
# writes the partition table with util-linux's lsblk, sfdisk and partx, which
# Fedora's container image leaves out.
server "sudo podman run --rm --privileged --pid=host --security-opt label=disable \
  -e SUDO_UID \
  -v /dev:/dev -v /sys:/sys \
  -v /sys/firmware/efi/efivars:/sys/firmware/efi/efivars \
  -v $guest_dir:$guest_dir:z -v /var/lib/tilo-imager:/var/lib/tilo-imager:z \
  registry.fedoraproject.org/fedora:44 bash -c '\
    dnf install -y -q --setopt=install_weak_deps=False util-linux >/dev/null \
    && exec $guest_dir/imager --tilo-staging-helper $guest_dir/06-request.json $guest_dir/06-progress.jsonl'" \
  > "$evidence/06-progress.stdout" 2> "$evidence/06-progress.stderr" \
  || { tail -n 60 "$evidence/06-progress.stderr" >&2; die 'the staging helper failed in its container'; }
server "cat $guest_dir/06-progress.jsonl" > "$evidence/06-progress.jsonl" || true
python3 - "$evidence/06-progress.jsonl" <<'PY' || die 'the staging helper did not finish ok'
import json, sys
lines = [line for line in open(sys.argv[1], encoding='utf-8').read().splitlines() if line.strip()]
last = json.loads(lines[-1]) if lines else {}
assert last.get('ok') is True, last
PY
server "sudo efibootmgr -v" > "$evidence/06-efibootmgr.txt"
grep -q 'Tilo installer' "$evidence/06-efibootmgr.txt" || die 'the staging wrote no Tilo installer boot entry'
grep -q 'BootNext: [0-9A-Fa-f]\{4\}' "$evidence/06-efibootmgr.txt" || die 'the staging wrote no BootNext'
server "sudo sfdisk --json /dev/vdb" > "$evidence/06-target-disks.json"
jq -e '.partitiontable.partitions | length == 2' "$evidence/06-target-disks.json" >/dev/null \
  || die 'the target disk does not carry exactly the two staging partitions'
note 'the machine is staged to boot its installer'
mark_step 06-staged ok

# --- reboot through BootNext; the installer installs with nobody there -------
phase=installer
# The reboot is asked of the machine's own systemd a moment from now, so the
# request answers before the machine goes down; it is asked again until it is
# taken, since the SSH that carries it may time out on a loaded machine. The
# firmware then loads the installer's entry through BootNext, which the
# serial shows.
rebooting=""
for attempt in $(seq 1 10); do
  if server 'sudo systemd-run --quiet --on-active=3 --unit=tilo-cycle-reboot systemctl reboot' \
      >> "$evidence/07-reboot.txt" 2>&1; then
    rebooting=1
    break
  fi
  sleep 3
done
[ -n "$rebooting" ] || { cat "$evidence/07-reboot.txt" >&2; die 'the machine did not take the reboot into its installer'; }
for i in $(seq 1 300); do
  tr -d '\000' < "$server_dir/serial.log" 2>/dev/null | grep -q 'Boot0100 "Tilo installer"' && break
  [ "$i" -lt 300 ] || {
    tr -d '\000' < "$server_dir/serial.log" | tail -n 40 >&2 || true
    die 'the firmware did not load the Tilo installer through BootNext'
  }
  sleep 1
done
note 'the machine boots its installer'
# The machine's asks reach the owner's session over the peer network. The
# install, its join and the sibling keyslot are one pass; each ask is answered
# where the person's devices answer questions.
await_owner_question() {
  local kind=$1 label=$2 listed="$out/owner-question-$2.json" resource=""
  # Seconds of the clock, not polls: each poll asks the owner over SSH.
  local deadline=$((SECONDS + ${TILO_SERVER_CYCLE_ASK_SECONDS:-1200}))
  while [ "$SECONDS" -lt "$deadline" ]; do
    owner_control questions --json > "$listed" 2>&1 || true
    resource="$(jq -r --arg kind "$kind" \
      'map(select(.kind == $kind))[0].resource // empty' "$listed" 2>/dev/null || true)"
    [ -z "$resource" ] || { printf '%s\n' "$resource"; return 0; }
    sleep 1
  done
  [ ! -f "$server_dir/serial.log" ] \
    || tr -d '\000' < "$server_dir/serial.log" | grep -viE 'recovery[ _-]?key' | tail -n 60 >&2 || true
  # The installer says what it is doing on its screen and in its journal,
  # never on the serial: the screen is what a person at the machine would see.
  local shot
  shot="$(server_vm shot "unasked-$label" 2>/dev/null)" && cp -f "$shot" "$out/server-screen-$label.png" || true
  die "the owner's sheet never asked a $kind question ($label)"
}
answer_owner() {
  local verdict=$1 resource=$2 action=answer
  [ "$verdict" = allow ] || action=cancel
  { owner_control raw "$(action_frame so.tilo.session "$resource" "$action")" \
      > "$out/owner-answer-$verdict.txt" 2>&1 \
      && action_completed "$out/owner-answer-$verdict.txt"; } \
    || { cat "$out/owner-answer-$verdict.txt" >&2; die "the owner's sheet could not be answered ($verdict)"; }
}
# The machine joins the owner's devices with the ticket the plan staged, which
# the owner's daemon serves: the person approved it when they planned the
# machine, so nobody is asked. Only a machine that joined can ask for the
# unlock keyslot, so that question is the join's proof.
sibling_question="$(await_owner_question network.device-enrol sibling)"
mark_step 07-joined ok
note "the owner approves the machine's unlock keyslot: $sibling_question"
answer_owner allow "$sibling_question"
mark_step 08-sibling ok

phase=first-boot
unlock_question="$(await_owner_question network.device-unlock first-boot)"
note "the owner releases the first boot's half: $unlock_question"
answer_owner allow "$unlock_question"
mark_step 09-first-unlock ok

# What the owner's device knows of the machine: its name, its device key and
# the recovery key the install handed over with the enrolment.
await_server_status() {
  local before_opened=${1:-} i output="" opened
  for i in $(seq 1 "${TILO_SERVER_CYCLE_STATUS_SECONDS:-1800}"); do
    output="$(owner_cmd tilo server status "$server_name" 2>&1 || true)"
    printf '%s\n' "$output" > "$out/server-status-latest.txt"
    opened="$(sed -n 's/^  opened: *//p' <<<"$output" | head -n 1)"
    if printf '%s\n' "$output" | grep -q 'opened:' \
      && printf '%s\n' "$output" | grep -q 'by you, on one of your devices' \
      && [ -n "$opened" ] \
      && { [ -z "$before_opened" ] || [ "$opened" != "$before_opened" ]; }; then
      printf '%s\n' "$output" > "$evidence/${2:-09-status}.txt"
      return 0
    fi
    sleep 1
  done
  printf '%s\n' "$output" >&2
  return 1
}
await_server_status '' 09-status || die 'the installed machine never came up unlocked'
note 'the installed machine is up and opened by its owner'
mark_step 09-first-boot ok

# --- Tilo becomes the default start, and a restart opens unattended ----------
phase=default-start
# The boot notes when it opened; the next boot's note names another moment,
# whatever time each took to reach the point where it answers.
first_opened="$(sed -n 's/^  opened: *//p' "$evidence/09-status.txt" | head -n 1)"
[ -n "$first_opened" ] || die 'the first status named no unlocked boot to tell the restart by'
owner_cmd tilo server set-default-start "$server_name" \
  > "$evidence/10-default-start.txt" 2>&1 \
  || { cat "$evidence/10-default-start.txt" >&2; die 'the machine did not take Tilo as its default start'; }
owner_cmd tilo server reboot "$server_name" > "$evidence/10-reboot.txt" 2>&1 \
  || { cat "$evidence/10-reboot.txt" >&2; die 'the machine did not take a restart'; }
phase=restart
second_question="$(await_owner_question network.device-unlock restart)"
note "the owner releases the restarted boot's half: $second_question"
answer_owner allow "$second_question"
await_server_status "$first_opened" 11-status || die 'the machine did not come up unlocked after the restart'
note 'the restarted machine is up and opened by its owner again'
mark_step 11-restart ok

# --- the recovery key is on the person's devices -----------------------------
phase=recovery-key
# The key itself is a secret: it is read outside the artifact directory and
# never copied into it, as the installer's serial is withheld for the same
# reason.
owner_cmd tilo server recovery-key "$server_name" --json \
  > "$storage/recovery-key.json" 2> "$evidence/12-recovery-key.stderr" \
  || { cat "$evidence/12-recovery-key.stderr" >&2; die 'the owner device kept no recovery key'; }
jq -e '.key | length > 16' "$storage/recovery-key.json" >/dev/null \
  || die 'the recovery key the owner device kept is empty'
jq -e '.label == $name' --arg name "$server_name" "$storage/recovery-key.json" >/dev/null \
  || die 'the saved recovery key names another machine'
jq '{machine, label, key_chars: (.key | length)}' "$storage/recovery-key.json" \
  > "$evidence/12-recovery-key.json"
note 'the owner device holds the recovery key'
mark_step 12-recovery-key ok

phase=done
printf 'PASS\n' > "$out/verdict.txt"
note 'the planned server installed unattended and opened with its owner on every boot'
