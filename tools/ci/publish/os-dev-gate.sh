#!/usr/bin/env bash
# What a development device needs of main's dev image before it follows it, checked the way a
# device takes it: the dev disk published last (os/dev/publish-dev-disk.sh) boots, stages this
# image with bootc and applies it with `tilo-extension live:system` (live, or recording that a
# restart applies it), restarts into it to a greeter that takes input and the device's own boot
# assessment (tilo-boot-health) accepts, and from there takes the next update live. As root in a
# micro-VM step with a /dev/kvm of its own. Nothing here holds a secret or publishes.
#
#   os-dev-gate.sh ARCH DEV OUT   DEV: os-dev-image-ARCH's layout; the logs into OUT
#
# A device falls back by itself from an image that does not reach a working greeter, so a
# failure the gate misses costs a dev device a reboot, not its data, as long as every published
# image boots and takes the update that fixes it. Installing an image onto a disk is
# os-dev-disk-ARCH's, which os-acceptance-ARCH boots.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-dev-gate.sh ARCH DEV OUT}
dev=${2:?}
out=${3:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch image is checked natively, not on $(uname -m)"
case $arch in
  x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga qemu-device-display-virtio-gpu) ;;
  aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu qemu-device-display-virtio-gpu-pci) ;;
  *) die "unknown architecture $arch" ;;
esac
[[ -f $dev/oci/index.json ]] || die "$dev holds no image layout (oci/)"
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'

missing=()
for p in socat qemu-img ipxe-roms-qemu sshpass openssh-clients jq procps-ng "${qemu[@]}"; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
command -v podman >/dev/null || missing+=(podman skopeo fuse-overlayfs e2fsprogs util-linux python3)
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
modprobe fuse 2>/dev/null || true
rm -rf "$out"
mkdir -p "$out"
revision=$(git rev-parse --verify HEAD) || die 'the gate names its commit: git = true'
digest=$(cat "$dev/digest")

# The disk the dev channel published last for ARCH: the newest main-<commit7>-ci version of the
# tilo-vm-disk package with this architecture's manifest, as `tilo-vm vm create` finds it.
packages=https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk
manifest=
for tag in $(curl -fsS --retry 3 "https://code.tilo.so/api/v1/packages/tilo?type=generic&q=tilo-vm-disk&limit=50" \
  | jq -r '[.[] | select(.name == "tilo-vm-disk" and (.version | test("^main-[0-9a-f]{7}-ci$")))]
           | sort_by(.created_at) | reverse | .[].version'); do
  manifest=$(curl -fsS --retry 3 "$packages/$tag/tilo-vm-disk-$arch.json") && break
  manifest=
done
[[ -n $manifest ]] || die "no dev disk is published for $arch to update from: publish-os-dev-disk-$arch publishes one"
old_digest=$(jq -r .imageDigest <<< "$manifest")
read -r url sha256 < <(jq -r '.disks[] | select(.format == "qcow2") | "\(.url) \(.sha256)"' <<< "$manifest")
printf 'updating from %s (%s, %s)\n' "$(jq -r .tag <<< "$manifest")" "$(jq -r .sourceRevision <<< "$manifest")" "$old_digest"
old=$scratch/published.qcow2
curl -fsSL --retry 3 -o "$old" "$url"
[[ $(sha256sum "$old" | cut -d' ' -f1) == "$sha256" ]] || die "$url is not the disk its manifest names"

# The updates, served by a registry on the host, which the guest reaches as 10.0.2.2: this dev
# image, and the next update, the same image with one file added.
podman run -d --name tilo-gate-registry -p 127.0.0.1::5000 docker.io/library/registry:2 >/dev/null
trap 'podman rm -f tilo-gate-registry >/dev/null 2>&1 || true' EXIT
port=$(podman port tilo-gate-registry 5000/tcp | head -n 1)
port=${port##*:}
[[ $port =~ ^[0-9]+$ ]] || die 'the update registry has no port'
for _ in $(seq 30); do curl -fs "http://127.0.0.1:$port/v2/" >/dev/null && break; sleep 1; done
update=gate-${revision:0:12}
next=live-update-check
probe=/usr/share/tilo/live-update-probe
printf 'live update of %s\n' "${revision:0:12}" > "$scratch/probe"
python3 -I - "$dev/oci" "$scratch/next" "$probe" "$scratch/probe" <<'PY'
# The next update: the dev image with one more layer, the probe. Its layout holds only what the
# dev image lacks; the registry has the dev image's layers already, so the copy sends no more.
import gzip, io, json, os, sys, tarfile
dev, root, path, content = sys.argv[1:]
sys.path.insert(0, "tools/ci/os")
import oci_layers as L
manifest, config = L.image_of_layout(dev)
data = open(content, "rb").read()
raw = io.BytesIO()
with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as tar:
    info = tarfile.TarInfo(path.lstrip("/"))
    info.size, info.mode = len(data), 0o644
    tar.addfile(info, io.BytesIO(data))
layer, size = L.put_blob(root, gzip.compress(raw.getvalue(), mtime=0))
manifest["layers"].append({"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": layer, "size": size})
config["rootfs"]["diff_ids"].append(L.sha256_bytes(raw.getvalue()))
config.setdefault("history", []).append({"created_by": "live-update-probe"})
digest, size = L.put_blob(root, L.canonical(config))
manifest["config"] = dict(manifest["config"], digest=digest, size=size)
digest, size = L.put_blob(root, L.canonical(manifest))
L.write_layout(root, {"mediaType": L.MANIFEST, "digest": digest, "size": size})
PY
skopeo copy -q --preserve-digests --dest-tls-verify=false "oci:$dev/oci" "docker://127.0.0.1:$port/tilo-vm:$update"
skopeo copy -q --preserve-digests --dest-tls-verify=false "oci:$scratch/next" "docker://127.0.0.1:$port/tilo-vm:$next"

# What this image's /usr/share/tilo-vm/base.json holds (its topmost layer writes it), which the
# live update must leave in place.
base_sha256=$(python3 -I - "$dev/oci" <<'PY'
import gzip, hashlib, sys, tarfile
sys.path.insert(0, "tools/ci/os")
import oci_layers
manifest, _ = oci_layers.image_of_layout(sys.argv[1])
for layer in reversed(manifest["layers"]):
    with gzip.open(oci_layers.blob_path(sys.argv[1], layer["digest"])) as f, tarfile.open(fileobj=f, mode="r|") as tar:
        for member in tar:
            if member.name.lstrip("./") == "usr/share/tilo-vm/base.json":
                print(hashlib.sha256(tar.extractfile(member).read()).hexdigest())
                sys.exit(0)
sys.exit("the dev image has no /usr/share/tilo-vm/base.json")
PY
)

export TILO_DISTROBOX= TILO_VM_ARCH=$arch TMPDIR=/var/tmp TILO_USER=quickuser TILO_PASS=tilo TILO_VM_DISPLAY=2d
for node in /dev/dri/renderD*; do
  [[ -c $node && -r $node && -w $node ]] && { TILO_VM_DISPLAY=gl; break; }
done
vm=os/dev/vm.sh
export TILO_RUN=/var/tmp/tilo-dev-gate-$arch TILO_SSH_PORT=2222
export TILO_DISK=$TILO_RUN/disk.qcow2
rm -rf -- "$TILO_RUN"
install -d -m 0700 "$TILO_RUN"
finish() {
  local rc=$?
  trap - EXIT
  if ((rc != 0)); then
    TILO_SSH_BOUND=60 "$vm" ssh 'systemctl --failed --no-pager; journalctl -b --no-pager -o short-monotonic -p warning -n 200' || true
    "$vm" serial 2>/dev/null | tail -n 80 || true
  fi
  "$vm" stop >/dev/null 2>&1 || true
  cp -f "$TILO_RUN/serial.log" "$out/" 2>/dev/null || true
  rm -rf -- "$TILO_RUN"
  podman rm -f tilo-gate-registry >/dev/null 2>&1 || true
  exit "$rc"
}
trap finish EXIT
as_root="printf '%s\\n' '$TILO_PASS' | sudo -S -p ''"
"$vm" tool qemu-img create -f qcow2 -F qcow2 -b "$old" "$TILO_DISK" >/dev/null
"$vm" boot "--$TILO_VM_DISPLAY"
"$vm" wait
echo 'ok    the published disk boots to a greeter'
"$vm" with-tests "$as_root \"\$TILO_GUEST_TESTS/live-update-check.sh\" --or-restart '10.0.2.2:$port/tilo-vm' registry.tilo.so/tilo/tilo-vm:$update /usr/share/tilo-vm/base.json $base_sha256"
echo 'ok    it stages this image and applies it'
"$vm" shutdown
"$vm" boot "--$TILO_VM_DISPLAY"
"$vm" wait
booted=$("$vm" ssh "$as_root bootc status --format json" | jq -r .status.booted.image.imageDigest)
[[ $booted == "$digest" ]] || die "it restarted into $booted, not this image ($digest)"
# The device's own boot assessment, which decides whether a boot is kept or falls back.
TILO_SSH_BOUND=780 "$vm" ssh "$as_root systemctl start tilo-boot-health.service" \
  || die 'tilo-boot-health failed: a device would fall back from this image'
echo 'ok    it restarts into this image, to a greeter that takes input, as the boot assessment requires'
"$vm" with-tests "$as_root \"\$TILO_GUEST_TESTS/live-update-check.sh\" '10.0.2.2:$port/tilo-vm' registry.tilo.so/tilo/tilo-vm:$next $probe $(sha256sum "$scratch/probe" | cut -d' ' -f1)"
echo 'ok    from it, it stages the next image and applies it live'
