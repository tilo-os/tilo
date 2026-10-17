#!/usr/bin/env bash
# Destructive loopback proof of the Imager staging helper (design spec §9).
#
# A sparse loop disk with a GPT ("retained ESP" + "retained root" + >120 GiB
# free), a fake efivarfs directory and a synthetic schema-2 bundle drive the
# real `imager --tilo-staging-request` and `imager --tilo-staging-helper`
# binaries through the real Linux adapter (sfdisk, partx, O_DIRECT I/O):
#
#   1. stage, killed with SIGKILL in the middle of write-root;
#   2. stage again with the same request: it must resume and complete;
#   3. assert the partition map (§2), both readbacks against the manifest
#      digests, the handoff (via the installer's own tilo-staging-handoff
#      `detect`), the owned Boot#### load option (decoded by the installer's
#      decoder), BootNext and a byte-identical BootOrder;
#   4. remove: the GPT areas and the efivars directory must be byte-identical
#      to the originals.
#
# Needs root and loop devices (CI disk runners). A run that cannot observe the
# real disk path fails instead of skipping. The imager must be a debug build
# (or built with TILO_IMAGER_STAGING_TEST_OVERRIDES=1) so the TILO_STAGING_TEST_*
# seams are honoured; release builds ignore them. Set IMAGER to a prebuilt
# binary, otherwise it is built here.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# The installer's handoff guard is compiled (crates/system/install-helpers);
# its fixture example takes the firmware variables and the ESP from the
# environment, and decodes a Boot#### variable as the installer does.
cargo build --quiet --locked --manifest-path "$repo/Cargo.toml" -p tilo-install-helpers \
  --example staging-handoff-fixture \
  || { echo 'FAIL  cannot build the staging handoff fixture' >&2; exit 1; }
handoff_helper="${CARGO_TARGET_DIR:-$repo/target}/debug/examples/staging-handoff-fixture"
fixture_manifest="$repo/os/tests/fixtures/internal-bundle-v2.json"

fail() { echo "FAIL  $*" >&2; exit 1; }
ok() { printf 'ok    %s\n' "$*"; }

[ "$(id -u)" -eq 0 ] || fail 'the staging helper loopback proof must run as root (it cannot observe a real GPT otherwise)'
for tool in losetup sfdisk partx lsblk blockdev truncate python3 sha256sum mkfs.vfat mmd mcopy dd; do
  command -v "$tool" >/dev/null 2>&1 || fail "missing required tool: $tool"
done
case "$(uname -m)" in
  x86_64) loader=BOOTX64.EFI ;;
  aarch64) loader=BOOTAA64.EFI ;;
  *) fail "unsupported architecture $(uname -m)" ;;
esac
arch="$(uname -m)"

if [ -z "${IMAGER:-}" ]; then
  (cd "$repo" && cargo build --locked -p imager --bin imager) || fail 'could not build the imager'
  IMAGER="$repo/target/debug/imager"
fi
[ -x "$IMAGER" ] || fail "imager binary not found: $IMAGER"

tmp="$(mktemp -d)"
chmod 700 "$tmp"
loop=""
helper_pid=""
cleanup() {
  [ -z "$helper_pid" ] || kill -9 "$helper_pid" 2>/dev/null || true
  [ -z "$loop" ] || losetup -d "$loop" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT

export MTOOLS_SKIP_CHECK=1
mib=2048 # 512-byte sectors per MiB

# --- synthetic bundle ----------------------------------------------------------
bundle="$tmp/bundle"
mkdir -p "$bundle/tree"
esp_img="$bundle/staging-esp.img"
root_img="$bundle/staging-root.img"
truncate -s 64M "$esp_img"
mkfs.vfat -F 32 -s 1 -S 512 -i ABCD1234 -n TILOSTAGE "$esp_img" >/dev/null
head -c 262144 /dev/urandom > "$bundle/tree/loader.efi"
python3 -c 'import sys; open(sys.argv[1], "wb").write(b" " * 65536)' "$bundle/tree/placeholder"
mmd -i "$esp_img" ::/EFI ::/EFI/BOOT ::/EFI/tilo-staging
mcopy -i "$esp_img" "$bundle/tree/loader.efi" "::/EFI/BOOT/$loader"
mcopy -i "$esp_img" "$bundle/tree/placeholder" ::/EFI/tilo-staging/handoff.json
mcopy -i "$esp_img" "$bundle/tree/placeholder" ::/EFI/tilo-staging/record.json
# Arbitrary bytes stand in for the minimised btrfs; the helper never parses
# the root. 1 GiB keeps write-root long enough to be interrupted reliably.
dd if=/dev/urandom of="$root_img" bs=4M count=256 status=none

python3 - "$fixture_manifest" "$bundle" "$arch" "$loader" <<'PY'
import hashlib, json, os, sys
fixture, bundle, arch, loader = sys.argv[1:]
m = json.load(open(fixture))
m['release_id'] = 'loop-test'
m['architecture'] = arch
m['boot_binding']['uki_architecture'] = arch
m['boot_binding']['efi_path'] = '/EFI/BOOT/' + loader
offset = 1 << 20
for c in m['components']:
    data = open(os.path.join(bundle, c['filename']), 'rb').read()
    c['byte_size'] = len(data)
    c['sha256'] = hashlib.sha256(data).hexdigest()
    c['source_offset'] = offset
    offset += len(data)
    if c['role'] == 'staging-esp':
        c['filesystem_uuid'] = 'ABCD-1234'
        c['minimum_partition_bytes'] = len(data)
    else:
        c['minimum_partition_bytes'] = 40 << 30  # the live root floor
m['source_image_bytes'] = offset + (1 << 20)
raw = json.dumps(m, separators=(',', ':')).encode()
open(os.path.join(bundle, 'manifest.json'), 'wb').write(raw)
catalog = {'schema': 1, 'tag': 'imager-v0.0.0-loop-test', 'releases': [{
    'purpose': 'tilo-internal-installer', 'layout': 'internal-bundle-v2',
    'release_id': 'loop-test', 'arch': arch,
    'manifest_url': 'https://code.tilo.so/tilo/loop-test/manifest.json',
    'manifest_sha256': hashlib.sha256(raw).hexdigest(), 'manifest_size': len(raw)}]}
json.dump(catalog, open(os.path.join(bundle, 'catalog.json'), 'w'))
PY
ok 'synthetic schema-2 bundle with blank ESP placeholders'

# --- loop disk and fake firmware ----------------------------------------------
truncate -s 160G "$tmp/disk.img"
loop="$(losetup --find --show --partscan "$tmp/disk.img")" || fail 'could not attach a loop device'
disk_guid=11111111-2222-4333-8444-555555555555
sfdisk --wipe always "$loop" >/dev/null <<EOF
label: gpt
label-id: $disk_guid
start=$((1 * mib)), size=$((600 * mib)), type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b, uuid=00000000-0000-4000-8000-00000000000a
start=$((601 * mib)), size=$((20 * 1024 * mib)), type=4f68bce3-e8cd-4db1-96e7-fbcaf984b709, uuid=00000000-0000-4000-8000-00000000000b
EOF
partx -u "$loop" 2>/dev/null || true
sectors="$(blockdev --getsz "$loop")"
gpt_areas() { # protective MBR + primary GPT, and the backup GPT
  dd if="$loop" bs=512 count=34 status=none | sha256sum | cut -d' ' -f1
  dd if="$loop" bs=512 skip=$((sectors - 33)) count=33 status=none | sha256sum | cut -d' ' -f1
}
gpt_areas > "$tmp/gpt.orig"
sfdisk --dump "$loop" > "$tmp/sfdisk.orig"

efivars="$tmp/efivars"
mkdir -p "$efivars"
python3 - "$efivars" <<'PY'
import struct, sys
g = '8be4df61-93ca-11d2-aa0d-00e098032b8c'
def var(name, payload):
    open('%s/%s-%s' % (sys.argv[1], name, g), 'wb').write(b'\x07\x00\x00\x00' + payload)
def option(desc):
    d = (desc + '\0').encode('utf-16-le')
    end = b'\x7f\xff\x04\x00'
    return struct.pack('<IH', 1, len(end)) + d + end
var('Boot0000', option('UEFI Shell'))
var('Boot0001', option('Retained OS'))
var('BootOrder', struct.pack('<HH', 1, 0))
var('BootCurrent', struct.pack('<H', 1))
var('SecureBoot', b'\x00')
PY
efivars_digest() { (cd "$efivars" && find . -type f -print0 | sort -z | xargs -0 sha256sum); }
efivars_digest > "$tmp/efivars.orig"
bootorder_orig="$(sha256sum < "$efivars/BootOrder-8be4df61-93ca-11d2-aa0d-00e098032b8c")"

export TILO_STAGING_TEST_DISK="$loop"
export TILO_STAGING_TEST_EFIVARS="$efivars"
export TILO_STAGING_TEST_RECORD_DIR="$tmp/records"
export TILO_STAGING_TEST_CATALOG="$bundle/catalog.json"

# --- request -------------------------------------------------------------------
work="$tmp/job"
mkdir -m 700 "$work"
"$IMAGER" --tilo-staging-request --manifest "$bundle/manifest.json" \
  --component "staging-esp=$esp_img" --component "staging-root=$root_img" \
  --target-bytes $((64 << 30)) --out "$work/request.json" > "$tmp/summary.json" \
  || fail "request builder refused: $(cat "$tmp/summary.json")"
operation="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["operation_id"])' "$work/request.json")"
ok "request built for operation $operation"

outcome() { python3 - "$1" <<'PY'
import json, sys
lines = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
last = lines[-1] if lines else {}
print('ok' if last.get('ok') is True else 'failed: %s' % last)
PY
}

# --- 1. stage, killed mid write-root ------------------------------------------
"$IMAGER" --tilo-staging-helper "$work/request.json" "$work/progress-1.jsonl" &
helper_pid=$!
killed=""
for _ in $(seq 1 6000); do
  if python3 - "$work/progress-1.jsonl" <<'PY'
import json, sys
try:
    lines = [json.loads(l) for l in open(sys.argv[1]) if l.endswith('\n')]
except FileNotFoundError:
    sys.exit(1)
sys.exit(0 if any(l.get('step') == 'write-root' and 0 < l.get('done_bytes', 0) < l.get('total_bytes', 0)
                  for l in lines) else 1)
PY
  then
    kill -9 "$helper_pid"
    killed=yes
    break
  fi
  kill -0 "$helper_pid" 2>/dev/null || break
  sleep 0.05
done
wait "$helper_pid" 2>/dev/null || true
helper_pid=""
[ -n "$killed" ] || fail "helper was never observed inside write-root: $(tail -n 3 "$work/progress-1.jsonl" 2>/dev/null)"
python3 - "$tmp/records/staging-status/$operation.json" <<'PY' || fail 'record does not show write-root in flight after SIGKILL'
import json, sys
r = json.load(open(sys.argv[1]))
states = {s['step']: s['state'] for s in r['steps']}
assert states.get('write-root') == 'intended', states
assert states.get('create-root') == 'done' and states.get('write-esp') == 'done', states
PY
ok 'helper killed with SIGKILL during write-root; record shows it intended'

# --- 2. resume -----------------------------------------------------------------
"$IMAGER" --tilo-staging-helper "$work/request.json" "$work/progress-2.jsonl" \
  || fail "resumed helper failed: $(outcome "$work/progress-2.jsonl")"
[ "$(outcome "$work/progress-2.jsonl")" = ok ] || fail "resumed helper: $(outcome "$work/progress-2.jsonl")"
ok 'the same request resumed and completed'

# --- 3. assertions -------------------------------------------------------------
sfdisk --json "$loop" > "$tmp/staged.json"
python3 - "$tmp/staged.json" "$work/request.json" "$bundle/manifest.json" <<'PY' || fail 'staged partition map violates spec §2'
import json, sys
table = json.load(open(sys.argv[1]))['partitiontable']
plan = json.load(open(sys.argv[2]))['plan']
manifest = json.load(open(sys.argv[3]))
mib = 2048
parts = {p['uuid'].lower(): p for p in table['partitions']}
assert len(parts) == 4, parts
orig = {'00000000-0000-4000-8000-00000000000a': (mib, 600 * mib),
        '00000000-0000-4000-8000-00000000000b': (601 * mib, 20 * 1024 * mib)}
for uuid, (start, size) in orig.items():
    assert (parts[uuid]['start'], parts[uuid]['size']) == (start, size), parts[uuid]
esp_guid, root_guid = plan['staging_esp']['partition_guid'], plan['staging_root']['partition_guid']
esp, root = parts[esp_guid], parts[root_guid]
assert esp['type'].lower() == 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b' and esp.get('name') == 'Tilo installer boot', esp
assert root['type'].lower() == 'd4c4daf6-f72c-43ee-b8e3-5ba32550311d' and root.get('name') == 'Tilo installer', root
target = plan['target_extent']
sizes = {c['role']: c['minimum_partition_bytes'] for c in manifest['components']}
assert esp['start'] == target['last_lba'] + 1, 'staging ESP follows the target extent'
assert root['start'] == esp['start'] + esp['size'], 'staging root follows the staging ESP'
assert esp['size'] * 512 == sizes['staging-esp'] and root['size'] * 512 == sizes['staging-root']
assert (target['last_lba'] - target['first_lba'] + 1) * 512 == 64 << 30
for first, count in ((target['first_lba'], target['last_lba'] - target['first_lba'] + 1),
                     (esp['start'], esp['size']), (root['start'], root['size'])):
    assert first % mib == 0 and count % mib == 0, (first, count)
retained_end = 601 * mib + 20 * 1024 * mib
assert target['first_lba'] >= retained_end + mib, 'target keeps 1 MiB clear of the retained root'
for p in parts.values():
    if p is esp or p is root:
        continue
    assert p['start'] + p['size'] <= target['first_lba'] - mib or p['start'] > root['start'] + root['size'] + mib
PY
ok 'partition map: target | staging ESP | staging root, aligned, clear of existing partitions'

esp_uuid="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["plan"]["staging_esp"]["partition_guid"])' "$work/request.json")"
root_uuid="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["plan"]["staging_root"]["partition_guid"])' "$work/request.json")"
node() { # partition node by PARTUUID, from the real table; no udev needed
  local uuid="$1" name
  name="$(python3 -c 'import json,os,sys
for p in json.load(open(sys.argv[1]))["partitiontable"]["partitions"]:
    if p["uuid"].lower() == sys.argv[2]: print(os.path.basename(p["node"]))' "$tmp/staged.json" "$uuid")"
  [ -n "$name" ] && [ -r "/sys/class/block/$name/dev" ] || return 1
  if [ ! -b "/dev/$name" ]; then
    mknod "$tmp/$name" b $(tr ':' ' ' < "/sys/class/block/$name/dev")
    echo "$tmp/$name"
  else
    echo "/dev/$name"
  fi
}
esp_dev="$(node "$esp_uuid")" || fail 'staging ESP not visible to the kernel'
root_dev="$(node "$root_uuid")" || fail 'staging root not visible to the kernel'

root_size="$(stat -c %s "$root_img")"
[ "$(head -c "$root_size" "$root_dev" | sha256sum | cut -d' ' -f1)" = "$(sha256sum < "$root_img" | cut -d' ' -f1)" ] \
  || fail 'staging root readback does not match the component digest'
python3 - "$esp_dev" "$esp_img" <<'PY' || fail 'staging ESP differs from its image outside the handoff placeholders'
import hashlib, sys
image = open(sys.argv[2], 'rb').read()
with open(sys.argv[1], 'rb') as dev:
    disk = dev.read(len(image))
changed = [i for i in range(0, len(image), 4096) if image[i:i + 4096] != disk[i:i + 4096]]
# Only placeholder bytes (blank in the image) may change: two 64 KiB files.
for block in changed:
    a, b = image[block:block + 4096], disk[block:block + 4096]
    assert all(x == y or x == 0x20 for x, y in zip(a, b)), 'changed a non-placeholder byte at %d' % block
assert 0 < len(changed) <= 2 * 65536 // 4096 + 2, len(changed)
masked = bytearray(disk)
for block in changed:
    for i in range(block, min(block + 4096, len(image))):
        if image[i] != disk[i]:
            masked[i] = 0x20
assert hashlib.sha256(bytes(masked)).hexdigest() == hashlib.sha256(image).hexdigest()
PY
ok 'readback: staging root equals its digest; staging ESP equals its digest with only the placeholders written'

esp_copy="$tmp/esp-copy"
mkdir -p "$esp_copy/EFI/tilo-staging"
mcopy -i "$esp_dev" ::/EFI/tilo-staging/handoff.json "$esp_copy/EFI/tilo-staging/handoff.json"
mcopy -i "$esp_dev" "::/EFI/BOOT/$loader" "$tmp/loader.readback"
cmp -s "$tmp/loader.readback" "$bundle/tree/loader.efi" || fail 'staged loader differs from the bundle'
detect_vars="$tmp/detect-efivars"
cp -a "$efivars" "$detect_vars"
python3 - "$detect_vars" "$esp_uuid" <<'PY'
import sys
open(sys.argv[1] + '/LoaderDevicePartUUID-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f', 'wb').write(
    b'\x06\x00\x00\x00' + (sys.argv[2].upper() + '\0').encode('utf-16-le'))
PY
TILO_STAGING_EFIVARS="$detect_vars" TILO_STAGING_ESP_DIR="$esp_copy" \
  "$handoff_helper" detect --out "$tmp/detected.json" --error "$tmp/detect-error.txt" \
  || fail 'tilo-staging-handoff detect crashed'
[ -s "$tmp/detected.json" ] || fail "installer rejected the handoff: $(cat "$tmp/detect-error.txt" 2>/dev/null)"
python3 - "$tmp/detected.json" "$work/request.json" "$loop" <<'PY' || fail 'handoff does not match the staged plan'
import json, subprocess, sys
h = json.load(open(sys.argv[1]))
plan = json.load(open(sys.argv[2]))['plan']
size = int(subprocess.check_output(['blockdev', '--getsize64', sys.argv[3]]))
assert h['disk_guid'] == '11111111-2222-4333-8444-555555555555' and h['disk_size_bytes'] == size
assert h['host_os'] == 'linux'
for key in ('staging_esp', 'staging_root'):
    assert h[key]['partuuid'] == plan[key]['partition_guid']
    assert (h[key]['first_lba'], h[key]['last_lba']) == (plan[key]['first_lba'], plan[key]['last_lba'])
assert h['target_extent'] == plan['target_extent']
assert h['original_boot_order'] == ['0001', '0000']
PY
owned="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["owned_boot_entry"])' "$tmp/detected.json")"
ok "installer's detect accepts the handoff (owned entry Boot$owned)"

decoded="$("$handoff_helper" decode-load-option "$efivars/Boot$owned-8be4df61-93ca-11d2-aa0d-00e098032b8c")" \
  || fail 'installer decoder could not read the owned boot entry'
python3 - "$decoded" "$efivars" "$owned" "$esp_uuid" "$loader" "$tmp/staged.json" <<'PY' || fail 'owned boot entry or BootNext is wrong'
import json, struct, sys
decoded, efivars, owned, esp_uuid, loader, staged = sys.argv[1:]
g = '8be4df61-93ca-11d2-aa0d-00e098032b8c'
raw = open('%s/Boot%s-%s' % (efivars, owned, g), 'rb').read()
assert raw[:4] == b'\x07\x00\x00\x00', raw[:4]
option = json.loads(decoded)
assert option is not None, 'installer decoder rejected the load option'
part = [p for p in json.load(open(staged))['partitiontable']['partitions'] if p['uuid'].lower() == esp_uuid][0]
digits = part['node'][len(part['node'].rstrip('0123456789')):]
assert option['partition_number'] == int(digits), (option, part['node'])
assert option['partuuid'] == esp_uuid, option
assert option['start_lba'] == part['start'] and option['size_lba'] == part['size'], option
assert option['path'] == '\\EFI\\BOOT\\' + loader, option
assert raw[4:8] == b'\x01\x00\x00\x00', 'LOAD_OPTION_ACTIVE'
assert raw[10:10 + len('Tilo installer') * 2] == 'Tilo installer'.encode('utf-16-le')
nxt = open('%s/BootNext-%s' % (efivars, g), 'rb').read()
assert nxt == b'\x07\x00\x00\x00' + struct.pack('<H', int(owned, 16)), nxt
PY
[ "$(sha256sum < "$efivars/BootOrder-8be4df61-93ca-11d2-aa0d-00e098032b8c")" = "$bootorder_orig" ] \
  || fail 'BootOrder changed'
ok "Boot$owned decodes to the staging ESP PARTUUID and loader; BootNext=$owned; BootOrder byte-identical"

# --- 4. remove -------------------------------------------------------------------
"$IMAGER" --tilo-staging-request --action remove --record "$operation" --out "$work/remove.json" \
  > "$tmp/remove-summary.json" || fail "remove request refused: $(cat "$tmp/remove-summary.json")"
"$IMAGER" --tilo-staging-helper "$work/remove.json" "$work/progress-remove.jsonl" \
  || fail "remove failed: $(outcome "$work/progress-remove.jsonl")"
partx -u "$loop" 2>/dev/null || true
gpt_areas > "$tmp/gpt.after"
if ! cmp -s "$tmp/gpt.orig" "$tmp/gpt.after"; then
  sfdisk --dump "$loop" | diff "$tmp/sfdisk.orig" - >&2 || true
  fail 'GPT after remove is not byte-identical to the original'
fi
efivars_digest > "$tmp/efivars.after"
diff "$tmp/efivars.orig" "$tmp/efivars.after" >&2 || fail 'efivars after remove differ from the original'
[ -z "$(ls "$tmp/records/staging-status" | grep -v "^$operation.json\$" || true)" ] || fail 'unexpected records'
python3 - "$tmp/records/staging-status/$operation.json" <<'PY' || fail 'record does not show every remove step done'
import json, sys
states = {s['step']: s['state'] for s in json.load(open(sys.argv[1]))['steps']}
for step in ('remove-boot-next', 'remove-boot-entry', 'remove-root', 'remove-esp', 'restore-shrink'):
    assert states.get(step) == 'done', states
PY
ok 'remove restored the GPT and efivars byte-for-byte'
