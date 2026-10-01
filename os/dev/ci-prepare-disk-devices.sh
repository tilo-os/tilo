#!/usr/bin/env bash
set -euo pipefail

# runner-large jobs execute inside disposable Kata guests. Their job container
# starts with a static /dev tmpfs even though the guest kernel has working FUSE
# and loop devices. Use the guest's devtmpfs so repart, losetup -P, and Podman
# see dynamically created loop partition nodes throughout the sealed VM gate.
[[ ${1:-} == --kata-job ]] || {
    echo 'usage: ci-prepare-disk-devices.sh --kata-job' >&2
    exit 2
}
sudo -n true
[[ -r /sys/class/misc/fuse/dev && -r /sys/class/misc/loop-control/dev ]] || {
    echo '::error::Kata guest kernel does not expose FUSE and loop-control' >&2
    exit 1
}
if [[ $(findmnt -n -o FSTYPE --target /dev) != devtmpfs ]]; then
    sudo -n mount -t devtmpfs devtmpfs /dev
fi
sudo -n mkdir -p /dev/pts /dev/shm
# Not `mountpoint`: over a fresh devtmpfs it still sees the devpts hidden
# underneath, so no pty could be opened (sshpass: "Failed to get a pseudo
# terminal: No such device") and every VM SSH command failed.
[ "$(stat -f -c %T /dev/pts)" = devpts ] || sudo -n mount -t devpts -o gid=5,mode=620,ptmxmode=666 devpts /dev/pts
mountpoint -q /dev/shm || sudo -n mount -t tmpfs -o mode=1777 tmpfs /dev/shm
sudo -n chmod 0666 /dev/fuse
[[ -c /dev/fuse && -c /dev/loop-control && -b /dev/loop0 ]] || {
    echo '::error::Kata devtmpfs has no FUSE or loop block device nodes' >&2
    exit 1
}
python3 -c 'open("/dev/fuse", "rb").close()'

# Probe actual loop attach and mount before the hours-long image build. A
# present character node alone would not prove this runner can install a disk.
scratch=$(mktemp -d "${RUNNER_TEMP:-/tmp}/tilo-disk-devices.XXXXXX")
loop=''
mounted=0
cleanup() {
    if [[ $mounted == 1 ]]; then sudo -n umount "$scratch/mnt"; fi
    if [[ -n $loop ]]; then sudo -n losetup -d "$loop"; fi
    rm -rf -- "$scratch"
}
trap cleanup EXIT
truncate -s 64M "$scratch/probe.raw"
loop=$(sudo -n losetup --find --show "$scratch/probe.raw")
[[ -b $loop ]] || {
    echo "::error::Kata loop device $loop is not a block node" >&2
    exit 1
}
sudo -n mkfs.ext4 -q "$loop"
mkdir "$scratch/mnt"
sudo -n mount "$loop" "$scratch/mnt"
mounted=1
mountpoint -q "$scratch/mnt"
echo '::notice::Kata FUSE and loop mount preflight passed'
