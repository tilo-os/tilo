#!/usr/bin/env bash
# The half of tools/ci/builder-vm.sh that runs inside its VM host container on the rcargo
# builder (Fedora with tools/ci/packages/vm-acceptance.txt, /dev/kvm passed through):
#
#   builder-vm-guest.sh worker       the worker VM the disks are installed in, booted from
#                                    /base/disk.qcow2 (main's published dev disk) with
#                                    /work/work.raw as its work disk, kept up until it has
#                                    made no disk for IDLE_MINUTES (45)
#   builder-vm-guest.sh disk LAYOUT OUT TREE
#                                    in the worker's container: the dev image in LAYOUT
#                                    installed onto a disk the way os-dev-disk-ARCH installs
#                                    it, by TREE's scripts, written to OUT/disk.raw with
#                                    OUT/disk.qcow2 over it and OUT/image-digest
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

on_worker() { as_user env TILO_VM=worker tilo-vm vm ssh -- "$@"; }
# rsync's remote shell: the worker's SSH, without the host name rsync puts first.
# rsync itself runs as root here, which reads every file of the tree.
printf '#!/bin/sh\nshift\nexec sudo -n -u %s -H env XDG_RUNTIME_DIR=/run/user/%s PATH="$PATH" TILO_VM=worker tilo-vm vm ssh -- "$@"\n' \
  "$user" "$uid" > /usr/local/bin/to-worker
chmod +x /usr/local/bin/to-worker
to_worker() { rsync -a --delete -e /usr/local/bin/to-worker "$@"; }
# The image goes into the worker, and the disk made of it comes out, through virtual disks
# of their own rather than over the worker's SSH (about 100 MB/s): the image's new blobs as
# a tar written onto the in disk, which the worker unpacks; the disk the size of the out
# disk, whose file is all holes before each install, the worker copying the data blocks of
# the raw disk it installed onto it, and the file kept out here, on the builder's own disk.
out_size=64G in_size=32G
in_dev=/dev/disk/by-id/virtio-tilo-in out_dev=/dev/disk/by-id/virtio-tilo-out

case ${1:-} in
  worker)
    # The builder has no loop devices and no root, so the install that os-dev-disk-ARCH runs
    # (tools/ci/publish/os-dev-disk.sh: os/dev/build-image.sh with TILO_SKIP_BUILD=1, onto a
    # loop-mounted disk) runs inside a VM instead: main's published dev disk (/base) booted
    # as the worker, as root there, with a work disk beside it. It stays up between disks,
    # which then start without a boot (tools/ci/builder-vm.sh starts it, and runs each disk
    # in it with `disk`); after IDLE_MINUTES without one it shuts down cleanly and exits.
    #
    # The work disk (/work/work.raw) outlives the worker, and holds what the install copies
    # the image through: the OCI layout, the podman store it is imported into (which root
    # podman runs bootc from too), the local registry the install pulls from, and the last
    # disk written, which the next starts as a copy of (disk-image.sh --seed-raw) so that its
    # composefs repository holds the shared layers already. Each of those takes only the
    # layers it lacks, so a disk for an image that differs from the last one in a few layers
    # moves and installs those layers alone, not the image's 12 GB four times over. Only the
    # image before keeps its layers there: what the next image shares with it.
    idle=${IDLE_MINUTES:-45}
    rm -f /work/ready
    work=/home/$user/disk
    install -d -o "$user" -g "$user" "$work"
    [[ -e /work/work.raw ]] || truncate -s 160G /work/work.raw
    rm -f /work/out.raw /work/in.raw
    truncate -s "$out_size" /work/out.raw
    truncate -s "$in_size" /work/in.raw
    chown "$user:" /work/work.raw /work/out.raw /work/in.raw
    as_user qemu-img create -q -f qcow2 -F qcow2 -b /base/disk.qcow2 "$work/base.qcow2"
    as_user tilo-vm vm create worker --image "$work/base.qcow2" --memory-mb 16384 --cpus 16
    # Read whole before matching: grep -m1 stopping early would kill virsh with SIGPIPE.
    domains=$(as_user virsh --connect qemu:///session list --all --name)
    domain=$(grep -m1 worker <<<"$domains")
    # Both scratch, so qemu leaves the guest's flushes to the builder's page cache (cache
    # unsafe): the image's layers move through podman, a registry and bootc's store, each
    # writing and syncing them, and with every flush reaching the builder's disk the worker
    # wrote 2 to 20 MB/s, which turned the import of an image into half an hour. What that
    # risks, a builder crash that loses writes the guest believed synced, is covered by the
    # clean-shutdown marker below.
    as_user virsh --connect qemu:///session attach-disk "$domain" /work/work.raw vdb \
      --targetbus virtio --driver qemu --subdriver raw --cache unsafe --config
    as_user virsh --connect qemu:///session attach-disk "$domain" /work/out.raw vdc \
      --targetbus virtio --driver qemu --subdriver raw --cache unsafe --serial tilo-out --config
    as_user virsh --connect qemu:///session attach-disk "$domain" /work/in.raw vdd \
      --targetbus virtio --driver qemu --subdriver raw --cache unsafe --serial tilo-in --config
    as_user tilo-vm vm start worker
    # The work disk is written through to its file only by a clean shutdown: a worker that
    # died with this container would leave its stores half written for the next disk, which
    # then starts them over (the stores-complete marker below).
    stop_worker() {
      rm -f /work/ready
      if on_worker 'sync; umount /var/w' >/dev/null 2>&1 \
         && as_user tilo-vm vm stop worker >/dev/null 2>&1; then
        touch /work/stopped-cleanly
      fi
    }
    trap 'stop_worker; exit 0' TERM INT
    trap stop_worker EXIT
    # The stores' caches and run state: the blob-info cache (which compressed blob each
    # layer already is, so a push skips what the registry has) on the work disk, run state
    # on tmpfs, where a store expects it after a boot.
    # A work disk the last worker did not unmount cleanly (the builder went down with it)
    # may miss writes its stores counted on: it starts over.
    fresh=1
    [[ -e /work/stopped-cleanly ]] && fresh=0
    rm -f /work/stopped-cleanly
    on_worker 'set -e
      if [ '"$fresh"' = 1 ] || ! blkid -t TYPE=btrfs /dev/vdb >/dev/null; then mkfs.btrfs -q -f /dev/vdb; fi
      mkdir -p /var/w; mount /dev/vdb /var/w
      mkdir -p /var/lib/containers
      rm -rf /var/lib/containers/cache; ln -s /var/w/blob-info-cache /var/lib/containers/cache
      # Images persist; containers live in run state alone (transient_store), so none is
      # left from a run before to refresh.
      printf "[storage]\ndriver = \"overlay\"\ngraphroot = \"/var/w/store\"\nrunroot = \"/run/w-store\"\ntransient_store = true\n" > /var/w/storage.conf
      rm -rf /var/w/root-podman /var/w/root-storage.conf'
    touch /work/last-disk /work/ready
    echo "builder-vm-guest: worker up"
    while sleep 30 & wait $!; do
      # What a disk leaves to prune (the image before it), done once no disk waits for the
      # worker, so that none waits for it.
      if [[ -e /work/prune ]] && exec 9>/work/lock && flock -n 9; then
        on_worker 'podman rm -f tilo-disk-registry >/dev/null 2>&1 || true
          CONTAINERS_STORAGE_CONF=/var/w/storage.conf TMPDIR=/var/w/tmp podman image prune -f >/dev/null' \
          && rm -f /work/prune
        exec 9>&-
      fi
      # Idle, and no disk being made (tools/ci/builder-vm.sh holds the lock for one): shut
      # down with the lock held, so that a disk waiting for it finds no worker, not a dying one.
      if [[ -n $(find /work/last-disk -mmin "+$idle") ]] && exec 9>/work/lock && flock -n 9; then
        echo "builder-vm-guest: no disk for ${idle} minutes; shutting the worker down"
        stop_worker
        exit 0
      fi
      exec 9>&-
    done
    ;;
  disk)
    # One disk, in the worker container (podman exec): the image layout in $2 installed onto
    # $3/disk.qcow2 with $3/image-digest and $3/install.log, by the tree in $4.
    layout=${2:?layout} out=${3:?output directory} src=${4:?tree}
    [[ -e /work/ready ]] || { echo "the worker is not up" >&2; exit 1; }
    touch /work/last-disk
    # The out disk's file all holes again: the copy below writes only the data blocks.
    fallocate --punch-hole --offset 0 --length "$(stat -c %s /work/out.raw)" /work/out.raw
    on_worker 'set -e
      rm -rf /var/w/dev-disk /var/w/dev-disk-run /var/w/tmp
      mkdir -p /var/w/tmp /var/w/store /var/w/src /var/w/image /var/w/registry /var/w/blob-info-cache
      # The stores are kept only from a disk that finished: one that died midway (this
      # marker missing) may have left them half written, and they start over.
      if [ ! -e /var/w/stores-complete ]; then
        echo "builder-vm-guest: the last disk did not finish; starting the stores over"
        rm -rf /var/w/store /var/w/registry /var/w/image /var/w/seed.raw*
        mkdir -p /var/w/store /var/w/registry /var/w/image
      fi
      rm -f /var/w/stores-complete; sync'
    echo "builder-vm-guest: sending what changed of the image and the tree"
    # The blobs the worker's layout lacks, as a tar on the in disk; those it holds that the
    # image no longer names go.
    on_worker 'ls /var/w/image/blobs/sha256 2>/dev/null || true' | sort > /tmp/have
    ls "$layout/image/blobs/sha256" | sort > /tmp/want
    fallocate --punch-hole --offset 0 --length "$(stat -c %s /work/in.raw)" /work/in.raw
    comm -13 /tmp/have /tmp/want | sed 's|^|blobs/sha256/|' \
      | tar -C "$layout/image" -c -f - oci-layout index.json -T - \
      | dd of=/work/in.raw conv=notrunc bs=4M status=none
    comm -23 /tmp/have /tmp/want | on_worker 'set -e
      mkdir -p /var/w/image; blockdev --flushbufs '"$in_dev"'
      tar -x -C /var/w/image -f '"$in_dev"'
      cd /var/w/image/blobs/sha256 && xargs -r rm -f'
    on_worker 'cat > /var/w/image.digest' < "$layout/image.digest"
    to_worker --exclude=/.git "$src/" worker:/var/w/src/
    echo "builder-vm-guest: image and tree in the worker; installing"
    on_worker 'set -e -o pipefail
      export CONTAINERS_STORAGE_CONF=/var/w/storage.conf TMPDIR=/var/w/tmp
      cd /var/w/src
      # (A run before this one may have left its prune to the worker, which a disk outruns.)
      podman rm -f tilo-disk-registry >/dev/null 2>&1 || true
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
        TILO_ROOT_PODMAN_STORAGE_CONF=/var/w/storage.conf \
        TILO_DISK_SIZE='"$out_size"' TILO_DISK_RAW=1 TILO_DISK_REG_PORT=5000 TILO_DISK_SEED_RAW=/var/w/seed.raw \
        TILO_DISK_PUBLISHED_DIGEST="$(cat /var/w/image.digest)" \
        TILO_TARGET_IMGREF=registry.tilo.so/tilo/tilo-vm:x86_64-ci \
        bash os/dev/build-image.sh 2>&1 | tee /var/w/install.log | grep --line-buffered -a "\[tilo\]" \
        || { tail -n 80 /var/w/install.log; exit 1; }'
    on_worker 'cat /var/w/install.log' > "$out/install.log"
    echo "builder-vm-guest: installed; copying the disk out"
    # Its data blocks only (the disk is mostly holes), several requests at a time.
    on_worker 'set -e; raw=/var/w/dev-disk/qcow2/disk.raw dev='"$out_dev"'
      [ "$(blockdev --getsize64 $dev)" = "$(stat -c %s "$raw")" ]
      qemu-img convert -n --target-is-zero -W -m 16 -f raw -O raw "$raw" $dev
      sync; blockdev --flushbufs $dev; rm -rf /var/w/dev-disk
      touch /var/w/stores-complete; sync'
    # Out here the disk is kept raw, a copy-on-write copy of the file (a reflink on the
    # builder's XFS: 26 s for a 7 GB disk, where converting it to qcow2 took 164 s), and its
    # disk.qcow2 is a qcow2 over it (backing file disk.raw, beside it) for what boots it.
    echo "builder-vm-guest: keeping the disk"
    cp --reflink=auto --sparse=always /work/out.raw "$out/disk.raw"
    (cd "$out" && qemu-img create -q -f qcow2 -F raw -b disk.raw disk.qcow2)
    [[ -s $out/disk.qcow2 ]] && qemu-img check -q "$out/disk.qcow2" \
      || { echo "the worker wrote no usable disk" >&2; exit 1; }
    cp "$layout/image.digest" "$out/image-digest"
    # Only the image just installed keeps its layers for the next disk: the worker prunes
    # the one before once this disk is out.
    touch /work/last-disk /work/prune
    ;;
  acceptance)
    # A writable copy of the tree the run may change (target/dev-runs, receipts).
    cp -a --reflink=auto /src-ro /src
    cd /src
    exec bash tools/ci/vm-acceptance.sh
    ;;
  *) echo "usage: builder-vm-guest.sh worker|disk LAYOUT OUT TREE|acceptance" >&2; exit 2 ;;
esac
