#!/usr/bin/env bash
# The half of tools/ci/builder-vm.sh that runs inside its VM host container on the rcargo
# builder (Fedora with tools/ci/packages/vm-acceptance.txt, /dev/kvm passed through):
#
#   builder-vm-guest.sh disk         the dev image in /layout installed onto a disk the way
#                                    os-dev-disk-ARCH installs it, in a worker VM booted from
#                                    /base/disk.qcow2 (main's published dev disk), written to
#                                    /out/disk.qcow2 with /out/image-digest
#   builder-vm-guest.sh acceptance   tools/ci/vm-acceptance.sh over the disk mounted where
#                                    that script reads it, for the shard in T_SHARDS
#
# The VM runs as an unprivileged user under libvirt's session daemon, as on a workstation.
set -euo pipefail

user=builder
id "$user" >/dev/null 2>&1 || useradd --create-home --shell /bin/bash "$user"
uid=$(id -u "$user")
install -d -m 0700 -o "$user" -g "$user" "/run/user/$uid"
install -d -o "$user" -g "$user" "/home/$user/.config" "/home/$user/.config/libvirt"
printf 'max_core = 0\n' > "/home/$user/.config/libvirt/qemu.conf"
chown "$user:" "/home/$user/.config/libvirt/qemu.conf"
as_user() { sudo -n -u "$user" -H env "XDG_RUNTIME_DIR=/run/user/$uid" "PATH=$PATH" "$@"; }

case ${1:-} in
  disk)
    # The builder has no loop devices and no root, so the install that os-dev-disk-ARCH runs
    # (tools/ci/publish/os-dev-disk.sh: os/dev/build-image.sh with TILO_SKIP_BUILD=1, onto a
    # loop-mounted disk) runs inside a VM instead: main's published dev disk booted as the
    # worker, as root there, with an empty work disk beside it that the worker formats. The
    # image layout and this tree's sources go in over its SSH, the finished disk comes out so.
    work=/home/$user/disk
    install -d -o "$user" -g "$user" "$work"
    rm -f /work/work.raw
    truncate -s 160G /work/work.raw
    chown "$user:" /work/work.raw
    as_user qemu-img create -q -f qcow2 -F qcow2 -b /base/disk.qcow2 "$work/base.qcow2"
    as_user tilo-vm vm create worker --image "$work/base.qcow2" --memory-mb 16384 --cpus 16
    domain=$(as_user virsh --connect qemu:///session list --all --name | grep -m1 worker)
    as_user virsh --connect qemu:///session attach-disk "$domain" /work/work.raw vdb \
      --targetbus virtio --driver qemu --subdriver raw --config
    as_user tilo-vm vm start worker
    on_worker() { as_user env TILO_VM=worker tilo-vm vm ssh -- "$@"; }
    on_worker 'set -e; mkfs.btrfs -q -f /dev/vdb; mkdir -p /var/w; mount /dev/vdb /var/w; mkdir -p /var/w/tmp /var/w/store /var/w/src'
    tar -C /layout -c image image.digest | on_worker 'tar -x -C /var/w'
    tar -C /src -c . | on_worker 'tar -x -C /var/w/src'
    on_worker 'set -e
      printf "[storage]\ndriver = \"overlay\"\ngraphroot = \"/var/w/store\"\nrunroot = \"/var/w/run\"\n" > /var/w/storage.conf
      export CONTAINERS_STORAGE_CONF=/var/w/storage.conf TMPDIR=/var/w/tmp
      cd /var/w/src
      id=$(podman pull -q oci:/var/w/image)
      [ "$(podman image inspect --format "{{.Digest}}" "$id")" = "$(cat /var/w/image.digest)" ]
      podman tag "$id" localhost/tilo-vm:ci
      export TILO_DISTROBOX= TILO_VM_ARCH=x86_64 TILO_USER=tester TILO_PASS=tilo SOURCE_DATE_EPOCH='"$SOURCE_DATE_EPOCH"'
      TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_OUTPUT=/var/w/dev-disk TILO_RUN=/var/w/dev-disk-run \
        TILO_ROOT_PODMAN_STORE=/var/w/root-podman TILO_DISK_SIZE=64G \
        TILO_TARGET_IMGREF=registry.tilo.so/tilo/tilo-vm:x86_64-ci \
        bash os/dev/build-image.sh > /var/w/install.log 2>&1 || { tail -n 80 /var/w/install.log; exit 1; }'
    on_worker 'cat /var/w/install.log' > /out/install.log
    on_worker 'cat /var/w/dev-disk/qcow2/disk.qcow2' > /out/disk.qcow2
    as_user tilo-vm vm stop worker
    [[ -s /out/disk.qcow2 ]] && qemu-img check -q /out/disk.qcow2 \
      || { echo "the worker wrote no usable disk" >&2; exit 1; }
    cp /layout/image.digest /out/image-digest
    rm -f /work/work.raw
    ;;
  acceptance)
    # A writable copy of the tree the run may change (target/dev-runs, receipts).
    cp -a --reflink=auto /src-ro /src
    cd /src
    exec bash tools/ci/vm-acceptance.sh
    ;;
  *) echo "usage: builder-vm-guest.sh disk|acceptance" >&2; exit 2 ;;
esac
