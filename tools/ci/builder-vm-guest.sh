#!/usr/bin/env bash
# The half of tools/ci/builder-vm.sh that runs inside its VM host container on the rcargo
# builder (Fedora with tools/ci/packages/vm-acceptance.txt, /dev/kvm passed through):
#
#   builder-vm-guest.sh disk         the dev image in /layout installed onto a disk the way
#                                    os-dev-disk-ARCH installs it, in a worker VM booted from
#                                    /base/disk.qcow2 (main's published dev disk), written to
#                                    /out/disk.qcow2 with /out/image-digest; /work keeps the
#                                    worker's work disk for the next run
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
    # worker, as root there, with a work disk beside it.
    #
    # The work disk (/work/work.raw) outlives the run, and holds what the install copies the
    # image through: the OCI layout, the podman store it is imported into, the local registry
    # the install pulls from and root podman's store bootc runs from, and the last disk
    # written, which the next starts as a copy of (disk-image.sh --seed-raw) so that its
    # composefs repository holds the shared layers already. Each of those takes only the
    # layers it lacks, so a disk for an image that differs from the last one in a few layers
    # moves and installs those layers alone, not the image's 12 GB four times over. Only the
    # image before keeps its layers there: what the next image shares with it.
    work=/home/$user/disk
    install -d -o "$user" -g "$user" "$work"
    if [[ ! -e /work/work.raw ]]; then
      truncate -s 160G /work/work.raw
    fi
    chown "$user:" /work/work.raw
    as_user qemu-img create -q -f qcow2 -F qcow2 -b /base/disk.qcow2 "$work/base.qcow2"
    as_user tilo-vm vm create worker --image "$work/base.qcow2" --memory-mb 16384 --cpus 16
    # Read whole before matching: grep -m1 stopping early would kill virsh with SIGPIPE.
    domains=$(as_user virsh --connect qemu:///session list --all --name)
    domain=$(grep -m1 worker <<<"$domains")
    as_user virsh --connect qemu:///session attach-disk "$domain" /work/work.raw vdb \
      --targetbus virtio --driver qemu --subdriver raw --config
    as_user tilo-vm vm start worker
    on_worker() { as_user env TILO_VM=worker tilo-vm vm ssh -- "$@"; }
    # The work disk is written through to its file only by a clean shutdown: a worker that
    # died with this container would leave its stores half written for the next run.
    stop_worker() {
      on_worker 'sync; umount /var/w' >/dev/null 2>&1 || true
      as_user tilo-vm vm stop worker >/dev/null 2>&1 || true
    }
    trap stop_worker EXIT
    # rsync's remote shell: the worker's SSH, without the host name rsync puts first.
    # rsync itself runs as root here, which reads every file of the tree.
    printf '#!/bin/sh\nshift\nexec sudo -n -u %s -H env XDG_RUNTIME_DIR=/run/user/%s PATH="$PATH" TILO_VM=worker tilo-vm vm ssh -- "$@"\n' \
      "$user" "$uid" > /usr/local/bin/to-worker
    chmod +x /usr/local/bin/to-worker
    to_worker() { rsync -a --delete -e /usr/local/bin/to-worker "$@"; }
    # The stores' caches and run state: the blob-info cache (which compressed blob each
    # layer already is, so a push skips what the registry has) on the work disk, run state
    # on tmpfs, where a store expects it after a boot.
    on_worker 'set -e
      blkid -t TYPE=btrfs /dev/vdb >/dev/null || mkfs.btrfs -q -f /dev/vdb
      mkdir -p /var/w; mount /dev/vdb /var/w
      rm -rf /var/w/dev-disk /var/w/dev-disk-run /var/w/tmp /var/w/root-podman/run
      mkdir -p /var/w/tmp /var/w/store /var/w/src /var/w/image /var/w/registry /var/w/blob-info-cache /var/lib/containers
      rm -rf /var/lib/containers/cache; ln -s /var/w/blob-info-cache /var/lib/containers/cache
      # Images persist; containers live in run state alone (transient_store), so none is
      # left from a run before to refresh. A store a crash broke anyway starts over.
      printf "[storage]\ndriver = \"overlay\"\ngraphroot = \"/var/w/store\"\nrunroot = \"/run/w-store\"\ntransient_store = true\n" > /var/w/storage.conf
      printf "[storage]\ndriver = \"overlay\"\ngraphroot = \"/var/w/root-podman/containers/storage\"\nrunroot = \"/var/w/root-podman/run\"\ntransient_store = true\n" > /var/w/root-storage.conf
      # The stores are kept only from a run that finished: one that died midway (this marker
      # missing) may have left them half written, and they start over.
      if [ ! -e /var/w/stores-complete ]; then
        echo "builder-vm-guest: the last run did not finish; starting the stores over"
        rm -rf /var/w/store /var/w/root-podman /var/w/registry /var/w/image /var/w/seed.raw*
        mkdir -p /var/w/store /var/w/registry /var/w/image
      fi
      rm -f /var/w/stores-complete'
    echo "builder-vm-guest: worker up; sending what changed of the image and the tree"
    to_worker /layout/image/ worker:/var/w/image/
    to_worker /layout/image.digest worker:/var/w/image.digest
    to_worker --exclude=/.git /src/ worker:/var/w/src/
    echo "builder-vm-guest: image and tree in the worker; installing"
    on_worker 'set -e -o pipefail
      export CONTAINERS_STORAGE_CONF=/var/w/storage.conf TMPDIR=/var/w/tmp
      cd /var/w/src
      # The registry disk-image.sh publishes the image to and installs from, kept on the
      # work disk with the layers of the image before (its manifests from earlier runs
      # are dropped first; the blobs it still tags stay).
      podman image exists docker.io/library/registry:2 || podman pull -q docker.io/library/registry:2 >/dev/null
      podman rm -f tilo-disk-registry >/dev/null 2>&1 || true
      # (Only once it has grown: a collection reads every blob and takes most of a minute.)
      if [ -d /var/w/registry/docker/registry/v2/repositories ] \
         && [ "$(du -sm /var/w/registry | cut -f1)" -gt 30000 ]; then
        podman run --rm --security-opt label=disable -v /var/w/registry:/var/lib/registry docker.io/library/registry:2 \
          garbage-collect --delete-untagged /etc/docker/registry/config.yml >/dev/null
      fi
      podman run -d --name tilo-disk-registry --security-opt label=disable -p 5000:5000 -v /var/w/registry:/var/lib/registry \
        -e REGISTRY_STORAGE_DELETE_ENABLED=true docker.io/library/registry:2 >/dev/null
      echo "builder-vm-guest: importing the image into the worker'"'"'s podman"
      if [ "$(podman image inspect --format "{{.Digest}}" localhost/tilo-vm:ci 2>/dev/null)" != "$(cat /var/w/image.digest)" ]; then
        id=$(podman pull -q oci:/var/w/image)
        [ "$(podman image inspect --format "{{.Digest}}" "$id")" = "$(cat /var/w/image.digest)" ]
        podman tag "$id" localhost/tilo-vm:ci
      fi
      # Into the registry as the layout holds it, its blobs as they are: disk-image.sh then
      # pushes nothing (TILO_DISK_PUBLISHED_DIGEST).
      skopeo copy -q --dest-tls-verify=false oci:/var/w/image docker://localhost:5000/tilo-vm:disk-install || true
      export TILO_DISTROBOX= TILO_VM_ARCH=x86_64 TILO_USER=tester TILO_PASS=tilo SOURCE_DATE_EPOCH='"$SOURCE_DATE_EPOCH"'
      TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_OUTPUT=/var/w/dev-disk TILO_RUN=/var/w/dev-disk-run \
        TILO_ROOT_PODMAN_STORE=/var/w/root-podman TILO_ROOT_PODMAN_STORAGE_CONF=/var/w/root-storage.conf \
        TILO_DISK_SIZE=64G TILO_DISK_REG_PORT=5000 TILO_DISK_SEED_RAW=/var/w/seed.raw \
        TILO_DISK_PUBLISHED_DIGEST="$(cat /var/w/image.digest)" \
        TILO_TARGET_IMGREF=registry.tilo.so/tilo/tilo-vm:x86_64-ci \
        bash os/dev/build-image.sh 2>&1 | tee /var/w/install.log | grep --line-buffered -a "\[tilo\]" \
        || { tail -n 80 /var/w/install.log; exit 1; }
      # Only the image just installed keeps its layers for the next run.
      podman rm -f tilo-disk-registry >/dev/null
      podman image prune -f >/dev/null
      CONTAINERS_STORAGE_CONF=/var/w/root-storage.conf TMPDIR=/var/w/root-podman/tmp podman image prune -f >/dev/null
      touch /var/w/stores-complete'
    on_worker 'cat /var/w/install.log' > /out/install.log
    echo "builder-vm-guest: installed; copying the disk out"
    on_worker 'cat /var/w/dev-disk/qcow2/disk.qcow2' > /out/disk.qcow2
    on_worker 'rm -rf /var/w/dev-disk'
    trap - EXIT
    stop_worker
    [[ -s /out/disk.qcow2 ]] && qemu-img check -q /out/disk.qcow2 \
      || { echo "the worker wrote no usable disk" >&2; exit 1; }
    cp /layout/image.digest /out/image-digest
    ;;
  acceptance)
    # A writable copy of the tree the run may change (target/dev-runs, receipts).
    cp -a --reflink=auto /src-ro /src
    cd /src
    exec bash tools/ci/vm-acceptance.sh
    ;;
  *) echo "usage: builder-vm-guest.sh disk|acceptance" >&2; exit 2 ;;
esac
