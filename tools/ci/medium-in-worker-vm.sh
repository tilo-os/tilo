#!/usr/bin/env bash
# dev-installer-medium-<arch> on a machine whose kernel can't build it, such as an rcargo builder:
# its kernel has no EROFS for composefs and no btrfs, and its rootless podman no loop devices.
# The check's own script, tools/ci/publish/dev-installer-medium.sh, runs unchanged in the
# project's builder image, as t's micro-VM ran it, inside a worker VM booted from main's
# newest published development disk (the generic package tilo-vm-disk-candidate), which has all
# three.
#
#   medium-in-worker-vm.sh ARCH IMAGE_DIR OUT
#
# IMAGE_DIR is os-image-ARCH's output (oci/ and digest), OUT receives what the check writes:
# disk.qcow2, its .sha256 and .hardware.json, and source.json. Run it from a committed checkout;
# the medium names its commit. It needs podman, /dev/kvm and the tilo-vm binary
# (tilo-vm-binary-<arch>'s output) in $TILO_VM_BIN. The worker's disk is fetched once into
# $TILO_WORKER_DISK_CACHE (default /var/tmp/tilo-worker-disk) and checked against its SHA-256.
#
# The worker runs in a container of Fedora with libvirt (tools/ci/packages/vm-acceptance.txt),
# as an unprivileged user under libvirt's session daemon; the tree and the image go in over its
# SSH, onto a work disk of its own, and the medium comes back the same way.
set -euo pipefail

if [[ ${1:-} == guest ]]; then
  # In the host container, as root: /base/disk.qcow2 the worker's disk, /layout IMAGE_DIR,
  # /src the checkout, /out OUT, /work scratch on the machine's disk.
  arch=$2 builder_image=$3
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
  printf '#!/bin/sh\nshift\nexec sudo -n -u %s -H env XDG_RUNTIME_DIR=/run/user/%s PATH="$PATH" TILO_VM=worker tilo-vm vm ssh -- "$@"\n' \
    "$user" "$uid" > /usr/local/bin/to-worker
  chmod +x /usr/local/bin/to-worker
  to_worker() { rsync -a --delete -e /usr/local/bin/to-worker "$@"; }

  disks=/home/$user/disk
  install -d -o "$user" -g "$user" "$disks"
  # The work disk holds the image's layers twice (the layout and the store) and the medium's
  # install: sparse, so it takes what is written.
  chmod 0755 /work
  truncate -s 200G /work/work.raw
  chown "$user:" /work/work.raw
  as_user qemu-img create -q -f qcow2 -F qcow2 -b /base/disk.qcow2 "$disks/worker.qcow2"
  cpus=$(( $(nproc) < 24 ? $(nproc) : 24 ))
  as_user tilo-vm vm create worker --image "$disks/worker.qcow2" --memory-mb 32768 --cpus "$cpus"
  domains=$(as_user virsh --connect qemu:///session list --all --name)
  domain=$(grep -m1 worker <<<"$domains")
  as_user virsh --connect qemu:///session attach-disk "$domain" /work/work.raw vdb \
    --targetbus virtio --driver qemu --subdriver raw --config
  trap 'as_user tilo-vm vm stop worker >/dev/null 2>&1 || true' EXIT
  as_user tilo-vm vm start worker
  on_worker 'set -e
    mkfs.btrfs -q -f /dev/vdb; mkdir -p /var/w; mount /dev/vdb /var/w
    mkdir -p /var/w/tmp /var/w/store /var/w/src /var/w/image /var/w/out \
      /var/w/inner/containers /var/w/inner/tmp /var/w/inner/var-tmp
    printf "[storage]\ndriver = \"overlay\"\ngraphroot = \"/var/w/store\"\nrunroot = \"/run/w-store\"\n" > /var/w/storage.conf'
  echo "medium-in-worker-vm: sending the image and the tree"
  to_worker /layout/ worker:/var/w/image/
  # The tree without its .git, which in a linked worktree is a file naming a directory elsewhere,
  # and as its .git a repository of its commit alone (/git): all the medium reads of git.
  to_worker --exclude /.git /src/ worker:/var/w/src/
  to_worker /git/ worker:/var/w/src/.git/
  echo "medium-in-worker-vm: building the medium in $builder_image"
  # The builder image privileged, with the worker's devices and root's podman store of its own,
  # as the check's micro-VM gave it.
  status=0
  on_worker "set -e -o pipefail
    export CONTAINERS_STORAGE_CONF=/var/w/storage.conf TMPDIR=/var/w/tmp
    podman pull -q $builder_image >/dev/null
    podman run --rm --privileged --network host --security-opt label=disable \
      -v /dev:/dev -v /var/w:/var/w -v /var/w/inner/containers:/var/lib/containers \
      -v /var/w/inner/tmp:/tmp -v /var/w/inner/var-tmp:/var/tmp -w /var/w/src \
      -e HOME=/root -e TMPDIR=/tmp \
      -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0=/var/w/src \
      --entrypoint bash $builder_image \
      -eo pipefail -c 'git reset -q && bash tools/ci/publish/dev-installer-medium.sh $arch /var/w/image /var/w/out' \
      > /var/w/medium.log 2>&1" || status=$?
  on_worker 'cat /var/w/medium.log' > /out/medium.log || true
  if ((status)); then
    tail -n 150 /out/medium.log
    echo "medium-in-worker-vm: the medium failed ($status); its log is in OUT/medium.log" >&2
    exit "$status"
  fi
  rsync -a -e /usr/local/bin/to-worker worker:/var/w/out/ /out/
  (cd /out && sha256sum -c --quiet disk.qcow2.sha256)
  exit 0
fi

cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
arch=${1:?usage: medium-in-worker-vm.sh ARCH IMAGE_DIR OUT}
layout=$(realpath "${2:?usage: medium-in-worker-vm.sh ARCH IMAGE_DIR OUT}")
out=${3:?usage: medium-in-worker-vm.sh ARCH IMAGE_DIR OUT}
case $arch in
  x86_64) ;;
  aarch64) ;;
  *) echo "unknown architecture: $arch" >&2; exit 2 ;;
esac
[[ $(uname -m) == "$arch" ]] || { echo "the $arch medium builds natively, not on $(uname -m)" >&2; exit 2; }
[[ -f $layout/oci/index.json && -f $layout/digest ]] || { echo "$layout is not os-image-$arch's output" >&2; exit 2; }
[[ -c /dev/kvm ]] || { echo "no /dev/kvm: the worker is a KVM guest" >&2; exit 2; }
vm_bin=${TILO_VM_BIN:?TILO_VM_BIN names the tilo-vm binary (tilo-vm-binary-<arch>)}
[[ -x $vm_bin ]] || { echo "$vm_bin is not executable" >&2; exit 2; }
[[ -z $(git status --porcelain --untracked-files=no) ]] \
  || { echo "the checkout has uncommitted changes; the medium is of a commit" >&2; exit 2; }

# The worker's disk: the newest development disk main published for this architecture (a
# tilo-vm-disk-candidate version named <commit>-<tooling>-<run>-<attempt>-<arch>; board disks
# add a suffix), checked against its candidate record. tilo-vm-disk-latest is not used: its
# x86_64 disk lags weeks behind, with a tilo-vmd too old for today's tilo-vm.
cache=${TILO_WORKER_DISK_CACHE:-/var/tmp/tilo-worker-disk}/$arch
mkdir -p "$cache"
read -r disk_file disk_sha disk_url < <(python3 - "$arch" <<'EOF'
import json, sys, urllib.request
arch = sys.argv[1]
api = "https://code.tilo.so/api/v1/packages/tilo?type=generic&q=tilo-vm-disk-candidate&limit=50&page={}"
newest = None
for page in range(1, 41):
    with urllib.request.urlopen(api.format(page), timeout=60) as reply:
        packages = json.load(reply)
    if not packages:
        break
    for p in packages:
        parts = p["version"].split("-")
        if p["name"] == "tilo-vm-disk-candidate" and len(parts) == 5 and parts[4] == arch:
            if newest is None or p["created_at"] > newest["created_at"]:
                newest = p
if newest is None:
    sys.exit(f"no published development disk for {arch}")
base = f"https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk-candidate/{newest['version']}"
with urllib.request.urlopen(f"{base}/candidate.json", timeout=60) as reply:
    disk = next(f for f in json.load(reply)["files"] if f["role"] == "disk")
print(disk["file"], disk["sha256"], disk["url"])
EOF
)
[[ -n ${disk_url:-} ]] || { echo "medium-in-worker-vm: found no worker disk for $arch" >&2; exit 1; }
if [[ $(cat "$cache/disk.sha256" 2>/dev/null) != "$disk_sha  $cache/$disk_file" ]]; then
  rm -f "$cache"/*.qcow2 "$cache/disk.sha256"
  echo "medium-in-worker-vm: fetching the worker's disk $disk_url"
  curl -fsSL --retry 3 -o "$cache/$disk_file" "$disk_url"
  echo "$disk_sha  $cache/$disk_file" | sha256sum -c --quiet
  echo "$disk_sha  $cache/$disk_file" > "$cache/disk.sha256"
fi

# The host container: Fedora with libvirt and QEMU, tagged by what it installs.
host_dir=$(mktemp -d)
trap 'rm -rf "$host_dir"' EXIT
mkdir -p "$host_dir/tools/ci/packages"
cp tools/ci/install-packages.sh "$host_dir/tools/ci/"
lists=(tools/ci/packages/vm-acceptance.txt)
if [[ $arch == aarch64 ]]; then lists+=(tools/ci/packages/worker-vm-aarch64.txt); fi
cp "${lists[@]}" "$host_dir/tools/ci/packages/"
names=$(printf '/tools/ci/packages/%s ' "${lists[@]##*/}")
cat > "$host_dir/Containerfile" <<EOF
FROM registry.fedoraproject.org/fedora:44
COPY tools /tools
RUN bash /tools/ci/install-packages.sh $names && dnf clean all
EOF
host_image=localhost/tilo-worker-vm-host:$(cat "$host_dir/Containerfile" "${lists[@]}" | sha256sum | cut -c1-16)
if ! podman image exists "$host_image"; then
  podman build -q -t "$host_image" "$host_dir" > "$host_dir/build.log" 2>&1 \
    || { tail -n 50 "$host_dir/build.log"; exit 1; }
fi

builder_image=$(python3 -c 'import sys; sys.path.insert(0, "tools/ci"); import resolve_builder; print(resolve_builder.resolve(""))')

mkdir -p "$out"
out=$(realpath "$out")
find "$out" -mindepth 1 -delete
work=$(mktemp -d "${TMPDIR:-/var/tmp}/medium-in-worker-vm.XXXXXX")
git init -q "$work/git"
git -C "$work/git" fetch -q --depth 1 "file://$PWD" HEAD
git -C "$work/git" update-ref HEAD FETCH_HEAD
name=medium-in-worker-vm-$$
trap 'podman rm -f "$name" >/dev/null 2>&1 || true; podman unshare rm -rf "$work"; rm -rf "$host_dir"' EXIT
podman run --rm --name "$name" --device /dev/kvm --security-opt label=disable \
  -v "$cache/$disk_file:/base/disk.qcow2:ro" -v "$work:/work" -v "$work/git/.git:/git:ro" -v "$out:/out" \
  -v "$layout:/layout:ro" -v "$PWD:/src:ro" -v "$(realpath "$vm_bin"):/usr/local/bin/tilo-vm:ro" \
  -v "$PWD/tools/ci/medium-in-worker-vm.sh:/medium-in-worker-vm.sh:ro" \
  "$host_image" bash /medium-in-worker-vm.sh guest "$arch" "$builder_image"
echo "medium-in-worker-vm: $out/disk.qcow2"
