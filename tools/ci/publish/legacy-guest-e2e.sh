#!/usr/bin/env bash
# The Linux legacy VM guest end to end: boot the disk image-legacy-guest built through
# `tilo-legacy` against a headless shell (os/dev/legacy-vm-e2e.sh): a window, typed text, a
# shared directory, suspend and resume, and a relay-launched app streamed to a member.
#
#   legacy-guest-e2e.sh GUEST BINARIES
#       GUEST holds image.oci.tar (legacy-guest.sh build), BINARIES the shell,
#       tilo-legacy and relay-it (legacy-guest.sh binaries)
#
# t runs it as root in a micro-VM step (isolation = "vm") with a /dev/kvm of its own, whose
# userspace is the CI builder, so the test runs right here rather than in a privileged
# container of that image. It is apart from legacy-guest.sh, an input of the guest build, so
# that changing the test does not rebuild the guest.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

guest=${1:?usage: legacy-guest-e2e.sh GUEST BINARIES}
binaries=${2:?usage: legacy-guest-e2e.sh GUEST BINARIES}
modprobe vhost_vsock 2>/dev/null || echo 'no vhost_vsock: the test uses vhost-device-vsock'
# The VM's /dev is static: the kernel's own nodes appear with devtmpfs, and the links a
# container runtime would add (libvirt reads /dev/stdin) are made here.
if [[ ! -c /dev/vhost-vsock ]] && ! mountpoint -q /dev; then
  mount -t devtmpfs devtmpfs /dev
  mkdir -p /dev/pts /dev/shm
  mount -t devpts -o gid=5,mode=620,ptmxmode=666 devpts /dev/pts
  mount -t tmpfs -o mode=1777 tmpfs /dev/shm
fi
[[ -e /dev/fd ]] || ln -s /proc/self/fd /dev/fd
for stream in stdin:0 stdout:1 stderr:2; do
  [[ -e /dev/${stream%:*} ]] || ln -s "/proc/self/fd/${stream#*:}" "/dev/${stream%:*}"
done
test -c /dev/kvm
# The test shares a directory under /var/tmp with the guest. virtiofsd opens the mount point
# of the filesystem a share is on, and tilo-legacy's Landlock confinement of it does not allow
# reading /var/tmp, which is a mount of its own in the VM ("Failed to reopen mount point").
# On the root filesystem, as in a container, the share works, so /var/tmp goes back onto it.
if mountpoint -q /var/tmp; then
  umount /var/tmp
  chmod 1777 /var/tmp
fi
work=$scratch/legacy-e2e
rm -rf "$work"
mkdir -p "$work/e2e/disk" "$work/e2e/bin" "$work/e2e/artifacts"
# The disk is the single layer of the wrapped image (FROM scratch, COPY disk.qcow2).
python3 - "$guest/image.oci.tar" "$work/e2e/disk" <<'EOF'
import json, sys, tarfile
archive, dest = sys.argv[1:]
with tarfile.open(archive) as oci:
    def blob(digest):
        return oci.extractfile("blobs/sha256/" + digest.split(":", 1)[1])
    index = json.load(oci.extractfile("index.json"))
    manifest = json.load(blob(index["manifests"][0]["digest"]))
    (layer,) = manifest["layers"]
    with tarfile.open(fileobj=blob(layer["digest"]), mode="r|*") as files:
        for member in files:
            if member.name.lstrip("./") == "disk.qcow2" and member.isfile():
                member.name = "disk.qcow2"
                files.extract(member, dest, filter="data")
                break
        else:
            sys.exit(f"{archive}: no disk.qcow2 in its layer")
EOF
test -s "$work/e2e/disk/disk.qcow2"
cp "$binaries"/{shell,tilo-legacy,relay-it} "$work/e2e/bin/"
cp -a "$repo_root/." "$work/src"
mv "$work/e2e" "$work/src/e2e"
chmod -R a+rwX "$work/src/e2e"
chmod a+rx "$work" "$work/src"
cd "$work/src"
status=0
bash os/dev/legacy-vm-e2e.sh || status=$?
if ((status != 0)); then
  # Nothing of the VM outlives the step: show what the test kept of its helpers and guest.
  for log in e2e/artifacts/*.log; do
    printf '\n== %s (end)\n' "$log"
    tail -n 80 "$log"
  done
fi
exit "$status"
