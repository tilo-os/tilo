#!/usr/bin/env bash
# Privileged end-to-end check of `os/dev/internal-bundle.py export` on real
# loop devices, vfat and Btrfs. Run as root on a Linux CI runner only:
#
#   sudo os/tests/internal_bundle_export.sh
#
# It builds a synthetic live-installer-shaped disk (GPT; FAT32 ESP holding a
# structural PE "UKI" with the derived live command line and a placeholder
# fallback loader; a 40 GiB sparse Btrfs Tilo root with some payload), exports
# it from raw and from qcow2, and checks the result with independent tools
# (blkid, btrfs check, a fresh read-only mount, uki-cmdline.py). The UKI is
# not bootable; boot is the VM round trip's job.
#
# It FAILS, never skips, when it cannot observe its subject: not root, not
# Linux, or a required tool missing.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
exporter="$repo/os/dev/internal-bundle.py"
fail() { printf 'FAIL  %s\n' "$*" >&2; exit 1; }
ok() { printf 'ok    %s\n' "$*"; }

[ "$(uname -s)" = Linux ] || fail "needs Linux loop devices; this is $(uname -s)"
[ "$(id -u)" = 0 ] || fail "needs root (loop devices, mounts, btrfs resize); rerun with sudo"
for tool in python3 sfdisk losetup mount umount mkfs.vfat mkfs.btrfs btrfs blkid qemu-img sha256sum truncate; do
  command -v "$tool" >/dev/null || fail "missing required tool: $tool"
done
python3 -c 'import sys, tomllib; sys.exit(sys.version_info < (3, 11))' \
  || fail "needs Python 3.11+ (tomllib) to derive the live command line"

work="$(mktemp -d /var/tmp/tilo-internal-bundle-export.XXXXXX)"
loops=()
mounts=()
cleanup() {
  local status=$?
  for ((i=${#mounts[@]}-1; i>=0; i--)); do umount "${mounts[$i]}" 2>/dev/null || true; done
  for loop in "${loops[@]}"; do losetup --detach "$loop" 2>/dev/null || true; done
  # Only remove the scratch tree when nothing is still mounted inside it.
  if ! grep -qF " $work/" /proc/self/mounts; then rm -rf -- "$work"; fi
  exit "$status"
}
trap cleanup EXIT

disk="$work/live.raw"
gib=$((1024 * 1024 * 1024))
root_bytes=$((40 * gib))
# 1 MiB + 64 MiB ESP + 40 GiB root + GPT backup slack.
truncate -s $((65 * 1024 * 1024 + root_bytes + 2 * 1024 * 1024)) "$disk"
sfdisk --quiet --label gpt "$disk" <<EOF
start=2048, size=131072, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI System"
start=133120, size=$((root_bytes / 512)), type=7A2E8D75-3F01-4E5A-B2C9-51F6ED90B8AB, name="Tilo root", attrs="GUID:59"
EOF

loop="$(losetup --find --show --partscan "$disk")"
loops+=("$loop")
udevadm settle 2>/dev/null || true
[ -b "${loop}p1" ] && [ -b "${loop}p2" ] || fail "partition devices did not appear for $loop"
mkfs.vfat -F 32 -i 1234ABCD -n TILOESP "${loop}p1" >/dev/null
mkfs.btrfs --quiet --force --label tilo-live "${loop}p2"
fsid="$(blkid -p -o value -s UUID "${loop}p2")"
[ -n "$fsid" ] || fail 'could not read the fixture Btrfs identity'
mkdir -p "$work/esp" "$work/root"
mount "${loop}p1" "$work/esp"; mounts+=("$work/esp")
mount "${loop}p2" "$work/root"; mounts+=("$work/root")
mkdir -p "$work/esp/EFI/BOOT" "$work/esp/EFI/Linux/bootc" "$work/esp/loader"
printf 'systemd-boot placeholder\n' > "$work/esp/EFI/BOOT/BOOTX64.EFI"
printf 'timeout 0\n' > "$work/esp/loader/loader.conf"
python3 - "$repo" "$work/esp/EFI/Linux/bootc/bootc_composefs-fixture.efi" <<'PY'
import importlib.util, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location('t', Path(sys.argv[1]) / 'os/tests/internal_bundle_test.py')
t = importlib.util.module_from_spec(spec)
spec.loader.exec_module(t)
Path(sys.argv[2]).write_bytes(bytes(t.pe_fixture(t.live_cmdline())))
PY
# Enough data that the minimum device size is well above zero.
head -c $((300 * 1024 * 1024)) /dev/urandom > "$work/root/payload"
mkdir -p "$work/root/usr/lib"; printf 'ID=tilo\n' > "$work/root/usr/lib/os-release"
sync
umount "$work/root"
umount "$work/esp"
mounts=()
losetup --detach "$loop"; loops=()
source_sha="$(sha256sum "$disk" | cut -d' ' -f1)"
ok 'built a live-installer-shaped disk'

check_bundle() {
  local out="$1" label="$2" manifest root_img esp_img
  manifest="$out/tilo-internal-installer-x86_64.json"
  root_img="$out/tilo-internal-installer-x86_64-root.img"
  esp_img="$out/tilo-internal-installer-x86_64-esp.img"
  [ -f "$manifest" ] && [ -f "$root_img" ] && [ -f "$esp_img" ] || fail "$label: bundle files missing"
  [ "$(find "$out" -mindepth 1 | wc -l)" = 3 ] || fail "$label: unexpected files in the bundle"
  python3 "$exporter" inspect --manifest "$manifest" >/dev/null || fail "$label: bundle does not inspect clean"
  python3 - "$manifest" "$root_bytes" <<'PY' || fail "$label: manifest facts"
import json, sys
m = json.load(open(sys.argv[1]))
esp, root = m['components']
assert (m['schema'], m['purpose']) == (2, 'tilo-internal-installer'), m
assert root['role'] == 'staging-root' and root['minimum_partition_bytes'] == int(sys.argv[2])
assert root['byte_size'] < root['minimum_partition_bytes'], 'root was not minimised'
assert root['byte_size'] % (1 << 20) == 0
assert esp['filesystem_uuid'] == '1234-ABCD', esp
b = m['boot_binding']
assert b['efi_path'] == '/EFI/BOOT/BOOTX64.EFI'
assert b['root_locator'] == 'UUID=' + root['filesystem_uuid']
assert ('root=UUID=%s ' % root['filesystem_uuid']) in b['uki_cmdline'] + ' '
assert 'tilo.root=' not in b['uki_cmdline']
PY
  local size fs_uuid got_uuid
  size="$(stat -c %s "$root_img")"
  fs_uuid="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["components"][1]["filesystem_uuid"])' "$manifest")"
  got_uuid="$(blkid -p -o value -s UUID "$root_img")"
  [ "$got_uuid" = "$fs_uuid" ] || fail "$label: blkid root UUID $got_uuid != manifest $fs_uuid"
  [ "$fsid" = "$fs_uuid" ] || fail "$label: root identity changed ($fsid -> $fs_uuid)"
  [ "$(blkid -p -o value -s UUID "$esp_img")" = 1234-ABCD ] || fail "$label: ESP serial changed"
  btrfs check --readonly "$root_img" >/dev/null 2>&1 || fail "$label: minimised root fails btrfs check"
  [ "$(btrfs inspect-internal dump-super "$root_img" | awk '$1 == "total_bytes" {print $2}')" = "$size" ] \
    || fail "$label: Btrfs size does not equal the truncated image"
  # A fresh read-only mount of the exported ESP, independent of the exporter.
  local esp_loop
  esp_loop="$(losetup --find --show --read-only "$esp_img")"; loops+=("$esp_loop")
  mkdir -p "$work/check-esp"
  mount -o ro "$esp_loop" "$work/check-esp"; mounts+=("$work/check-esp")
  cmp -s "$work/check-esp/EFI/BOOT/BOOTX64.EFI" "$work/check-esp/EFI/Linux/bootc/bootc_composefs-fixture.efi" \
    || fail "$label: fallback loader is not the specialised UKI"
  [ "$(sha256sum "$work/check-esp/EFI/BOOT/BOOTX64.EFI" | cut -d' ' -f1)" = \
    "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["boot_binding"]["uki_sha256"])' "$manifest")" ] \
    || fail "$label: manifest uki_sha256 does not name the fallback loader"
  python3 "$repo/os/dev/uki-cmdline.py" show "$work/check-esp/EFI/BOOT/BOOTX64.EFI" \
    | tr ' ' '\n' | grep -x "root=UUID=$fs_uuid" >/dev/null || fail "$label: fallback loader is not bound to the staging root"
  [ "$(cat "$work/check-esp/loader/loader.conf")" = 'timeout 0' ] || fail "$label: unrelated ESP files changed"
  umount "$work/check-esp"; mounts=()
  losetup --detach "$esp_loop"; loops=()
  ok "$label: bundle verified independently ($((size / 1024 / 1024)) MiB root image for a 40 GiB partition)"
}

python3 "$exporter" export --image "$disk" --arch x86_64 --release fixture-export \
  --output "$work/raw-bundle" >/dev/null || fail 'raw export failed'
check_bundle "$work/raw-bundle" raw
[ "$(sha256sum "$disk" | cut -d' ' -f1)" = "$source_sha" ] || fail 'export modified its source image'
ok 'raw source image unchanged'

qemu-img convert -f raw -O qcow2 "$disk" "$work/live.qcow2"
python3 "$exporter" export --image "$work/live.qcow2" --arch x86_64 --release fixture-export \
  --output "$work/qcow2-bundle" >/dev/null || fail 'qcow2 export failed'
check_bundle "$work/qcow2-bundle" qcow2
[ -z "$(find "$work" -maxdepth 1 -name '.tilo-bundle-work-*')" ] || fail 'export left its work directory'

# Refusals: an existing output, and a non-root caller, create nothing.
if python3 "$exporter" export --image "$disk" --arch x86_64 --release fixture-export \
     --output "$work/raw-bundle" >/dev/null 2>&1; then
  fail 'export overwrote an existing bundle'
fi
if command -v setpriv >/dev/null; then
  # A readable, traversable scratch area so the refusal is about privilege,
  # not about reading the input.
  chmod 0711 "$work"
  mkdir -m 0777 "$work/unprivileged"
  head -c $((1024 * 1024)) /dev/zero > "$work/unprivileged/disk.raw"
  chmod 0644 "$work/unprivileged/disk.raw"
  if setpriv --reuid=65534 --regid=65534 --clear-groups python3 "$exporter" export \
       --image "$work/unprivileged/disk.raw" --arch x86_64 --release fixture-export \
       --output "$work/unprivileged/out" 2>"$work/unprivileged.err"; then
    fail 'unprivileged export succeeded'
  fi
  grep -q 'needs root' "$work/unprivileged.err" || fail 'unprivileged export did not say it needs root'
  [ "$(ls -A "$work/unprivileged")" = disk.raw ] || fail 'unprivileged export created output'
  ok 'unprivileged export refuses before creating anything'
else
  fail 'missing required tool: setpriv'
fi
ok 'internal bundle export'
