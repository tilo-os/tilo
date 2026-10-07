#!/usr/bin/env bash
# Assemble an existing signed Orange Pi child. No OS rebuild or account seed.
set -euo pipefail
[[ $(uname -m) == aarch64 ]]
[[ ${IMAGE:-} =~ ^registry\.tilo\.so/tilo/tilo-vm@sha256:[0-9a-f]{64}$ ]]
[[ ${IMAGE_SOURCE:-} =~ ^[0-9a-f]{40}$ ]]
: "${OUTPUT:?}"
mkdir -p "$OUTPUT"
cosign verify --key keys/cosign.pub "$IMAGE" > "$OUTPUT/signature.json"
cosign verify-attestation --key keys/cosign.pub --type slsaprovenance1 "$IMAGE" > "$OUTPUT/provenance.json"
cosign verify-attestation --key keys/cosign.pub --type https://tilo.so/attestations/development-board-freshness/v1 "$IMAGE" > "$OUTPUT/freshness.json"
python3 - "$OUTPUT" "$IMAGE" "$IMAGE_SOURCE" <<'PY'
import base64, json, sys, time
from pathlib import Path
out, image, source = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
repository, digest = image.split('@sha256:')
def statements(name):
    data = json.loads((out / name).read_text())
    if not isinstance(data, list): data = [data]
    return [json.loads(base64.b64decode(x['payload'])) for x in data]
def bound(statement):
    return any(x['name'] == repository and x.get('digest', {}).get('sha256') == digest for x in statement['subject'])
assert any(bound(x) and x['predicate']['buildDefinition']['buildType'] == 'https://tilo.so/buildtypes/bootc-development-board/v1'
           and x['predicate']['buildDefinition']['externalParameters']['sourceRevision'] == source
           and x['predicate']['buildDefinition']['externalParameters']['architecture'] == 'aarch64'
           and x['predicate']['buildDefinition']['externalParameters']['hardwareProfile'] == 'orangepi-3b-v2-1'
           for x in statements('provenance.json'))
assert any(bound(x) and x['predicate']['imageDigest'] == 'sha256:' + digest
           and x['predicate']['sourceRevision'] == source and x['predicate']['repository'] == repository
           and x['predicate']['hardwareProfile'] == 'orangepi-3b-v2-1'
           and x['predicate']['issuedAt'] <= time.time() < x['predicate']['expiresAt']
           for x in statements('freshness.json'))
print('Signed immutable source, ARM architecture, Orange Pi profile and freshness verified')
PY
skopeo inspect --raw "docker://$IMAGE" > "$OUTPUT/image-manifest.json"
actual="sha256:$(sha256sum "$OUTPUT/image-manifest.json" | awk '{print $1}')"
[[ $IMAGE == "registry.tilo.so/tilo/tilo-vm@$actual" ]]
# The existing image builder establishes the same 16 MiB Rockchip gap and
# systemd-boot chain. The physical device's already-working loader is retained
# separately by the operator, rather than rebuilt or uploaded to this runner.
# A 58 GiB image fits the observed 63,281,561,600-byte card and starts with
# enough root space for a retained signed update. No first-boot growth claim.
TILO_DISTROBOX= os/dev/disk-image.sh "$IMAGE" "$OUTPUT/install" \
  --composefs --bootloader systemd --size 58G --root-min 54G --esp 2G \
  --boot-gap 16M --target-imgref "$IMAGE" --sealed-source-imgref "$IMAGE" --raw
loop=$(sudo -n losetup --find --show --partscan "$OUTPUT/install/disk.raw")
mkdir -p "$OUTPUT/root" "$OUTPUT/esp"
cleanup() {
  sudo -n umount "$OUTPUT/esp" 2>/dev/null || true
  sudo -n umount "$OUTPUT/root" 2>/dev/null || true
  sudo -n losetup -d "$loop"
}
trap cleanup EXIT
sudo -n mount -o ro,rescue=nologreplay "${loop}p3" "$OUTPUT/root"
sudo -n mount -o ro "${loop}p2" "$OUTPUT/esp"
sudo -n sfdisk --json "$loop" > "$OUTPUT/partition-table.json"
sudo -n python3 os/dev/check-composefs-verity.py "$OUTPUT/root/composefs/objects"
sudo -n python3 - "$OUTPUT" "$IMAGE" "$IMAGE_SOURCE" "${GITHUB_SHA:?}" <<'PY'
import hashlib, json, os, struct, sys
from pathlib import Path
out = Path(sys.argv[1]); image, source, tooling = sys.argv[2:]
layout = json.loads((out / 'partition-table.json').read_text())['partitiontable']['partitions']
assert [x['start'] for x in layout] == [2048, 32768, 4227072]
assert layout[2]['size'] * 512 > 54 * 1024**3
root = out / 'root'; esp = out / 'esp'
def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(4*1024**2), b''): h.update(block)
    return h.hexdigest()
boot = esp / 'EFI/BOOT/BOOTAA64.EFI'
assert boot.is_file() and boot.stat().st_size > 0
ukis = list((esp / 'EFI/Linux').rglob('*.efi')); assert len(ukis) == 1
origins = list((root / 'state/deploy').rglob('*.origin')); assert len(origins) == 1
assert image in origins[0].read_text()
assert (esp / 'loader/loader.conf').is_file()
free = os.statvfs(root).f_bavail * os.statvfs(root).f_frsize
assert free > 32 * 1024**3
proof = {'schema':1,'image':image,'source':source,'tooling':tooling,
         'purpose':'signed Orange Pi disk assembly; physical acceptance pending',
         'virtual_bytes':(out/'install/disk.raw').stat().st_size,'root_free_bytes':free,
         'partition_layout':layout,'uki_sha256':sha(ukis[0]),'bootloader_sha256':sha(boot),
         'composefs_verity':'all installed objects verified','signature_policy':'authoritative signed source enforced; unchanged installed policy',
         'account_credentials_added':False,'existing_working_rockchip_loader_required':True,
         'physical_card_personalization_required':['retained boot gap','existing network and public SSH key','existing identity state','explicitly approved remote administrator grant'],
         'hardware_boot_claim':False}
(out / 'preparation.json').write_text(json.dumps(proof,indent=2)+'\n')
print(json.dumps(proof))
PY
cleanup
trap - EXIT
qemu-img convert -c -f raw -O qcow2 -o compression_type=zstd "$OUTPUT/install/disk.raw" "$OUTPUT/modern-orangepi-signed.qcow2"
qemu-img check -f qcow2 "$OUTPUT/modern-orangepi-signed.qcow2"
(cd "$OUTPUT" && sha256sum modern-orangepi-signed.qcow2 > modern-orangepi-signed.qcow2.sha256)
