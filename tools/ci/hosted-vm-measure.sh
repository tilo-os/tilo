#!/usr/bin/env bash
# What a workspace's VM costs with its Browser open: the VM host image, as a
# node runs it, with the guest image this checkout builds, on a machine with
# KVM. It wakes one workspace's VM cold, waits for the host to make the
# template of its size, then wakes CLONES more as clones of it, opens a page in
# Chromium in each (tools/ci/hosted-browser-probe.py), and reports, as JSON on
# the log and in OUT/report.json:
#
#   - each wake: how long, and whether it restored a clone;
#   - each page: milliseconds from starting the engine to its first frame;
#   - each VM's memory on the host with its page open (PSS, which splits what
#     clones share between them, and RSS), cold and as a clone;
#   - how many such clones a D4s_v5's VM budget (12 GiB) holds.
#
# usage: hosted-vm-measure.sh VM_HOST_ARCHIVE GUEST_DIR OUT [CLONES]
# As root in a micro-VM step with /dev/kvm (t.toml: hosted-vm-measure).
set -euo pipefail
archive=${1:?usage: hosted-vm-measure.sh VM_HOST_ARCHIVE GUEST_DIR OUT [CLONES]}
guest_dir=${2:?}
out=${3:?}
clones=${4:-4}
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
. "$here/publish/lib.sh"

dnf install -y --setopt=install_weak_deps=False podman jq xfsprogs util-linux tar >/dev/null
work=$scratch/hosted-vm-measure
rm -rf "$work"
mkdir -p "$work" "$out"

# The guest where fetch-guest finds it already fetched: its digest's directory.
digest=$(cat "$guest_dir/digest")
layers=$work/guest-oci
mkdir -p "$layers"
tar -xf "$guest_dir/image.oci.tar" -C "$layers"
layer=$(jq -r '.layers[0].digest' "$layers/blobs/sha256/$(jq -r '.manifests[0].digest' "$layers/index.json" | cut -d: -f2)" | cut -d: -f2)

# The state disk a node gives the host: XFS with reflink, on a loop device.
truncate -s 60G "$work/state.img"
mkfs.xfs -q -m reflink=1 -L tilovmstate "$work/state.img"
mkdir -p "$work/mnt"
mount -o loop "$work/state.img" "$work/mnt"
images=$work/mnt/images/hosted-${digest#sha256:}
mkdir -p "$images"
tar -xf "$layers/blobs/sha256/$layer" -C "$images" vmlinux rootfs.ext4
chmod 0444 "$images/vmlinux" "$images/rootfs.ext4"
chown -R 10001:10001 "$work/mnt/images"
umount "$work/mnt"
device=$(losetup --find --show "$work/state.img")

printf '{"default":{"max_running":32,"max_vms":64,"sizes":["small","default"]}}\n' > "$work/plans.json"
image=$(podman load -q -i "$archive" | sed -n 's/^Loaded image: //p' | tail -1)
podman run -d --name vm-host --privileged --cgroupns=private \
  --device /dev/kvm --device /dev/net/tun --device "$device:/dev/tilo-vm-state" \
  -v "$work/plans.json:/etc/tilo/plans.json:ro" \
  -e TILO_VM_GUEST_REF="registry.tilo.so/tilo/hosted-guest@$digest" \
  -e TILO_VM_PLANS=/etc/tilo/plans.json \
  "$image" >/dev/null
# A failed check keeps no outputs, so what the host and its VMs said goes to
# the log: the console log is where Firecracker and the jail helper write why
# a VM stopped.
finish() {
  local status=$?
  if [[ $status != 0 ]]; then
    podman logs --tail 100 vm-host >&2 || true
    podman exec vm-host sh -c 'for log in /var/lib/tilo-hosted/state/vms/*/*/console.log; do
        [ -f "$log" ] || continue
        echo "== $log"
        tail -n 80 "$log"
      done
      cat /sys/fs/cgroup/cgroup.controllers; ls -l /dev/net/tun /dev/kvm' >&2 || true
  fi
  podman rm -f vm-host >/dev/null 2>&1 || true
  losetup -d "$device" || true
  return "$status"
}
trap finish EXIT

inside() { podman exec vm-host "$@"; }
as_service() {
  inside env HOME=/var/lib/tilo-hosted setpriv --reuid=10001 --regid=10001 --init-groups -- "$@"
}
for _ in $(seq 120); do
  inside test -S /run/tilo-hosted/hosted.sock && break
  sleep 1
done
inside test -S /run/tilo-hosted/hosted.sock || die 'the VM host never served its socket'

probe=$(cat "$here/hosted-browser-probe.py")
# Open a page in WORKSPACE's VM, keep it open, and wait for the probe's line.
open_page() {
  local workspace=$1
  as_service tilo-legacy hosted run --run /run/tilo-hosted --workspace "$workspace" \
    --account acct-measure --size small -- python3 -c "$probe" 600 > "$work/$workspace.out" 2>&1 &
  for _ in $(seq 300); do
    grep -q '^{' "$work/$workspace.out" 2>/dev/null && break
    sleep 1
  done
  grep -m1 '^{' "$work/$workspace.out" || die "$workspace: no page: $(tail -5 "$work/$workspace.out")"
}

# Memory of every VM on the host, by its Firecracker process, read from this
# machine's /proc (the container shares its kernel).
weigh() {
  for proc in /proc/[0-9]*; do
    [[ $(cat "$proc/comm" 2>/dev/null) == firecracker ]] || continue
    awk -v pid="${proc#/proc/}" '/^Rss:/ { rss = $2 } /^Pss:/ { pss = $2 }
      END { printf "{\"pid\":%s,\"pss_kib\":%d,\"rss_kib\":%d}\n", pid, pss, rss }' \
      "$proc/smaps_rollup"
  done | jq -s .
}

status() { as_service tilo-legacy hosted status --run /run/tilo-hosted; }

cold_page=$(open_page ws-cold)
cold_weight=$(weigh)
for _ in $(seq 600); do
  inside test -f /var/lib/tilo-hosted/state/templates/small/template && break
  sleep 1
done
inside test -f /var/lib/tilo-hosted/state/templates/small/template || die 'no template was made'

pages=()
for n in $(seq "$clones"); do
  pages+=("$(open_page "ws-$n")")
done
sleep 10
clone_weight=$(weigh)

jq -n \
  --argjson cold_page "$cold_page" \
  --argjson cold_weight "$cold_weight" \
  --argjson clone_pages "$(printf '%s\n' "${pages[@]}" | jq -s .)" \
  --argjson clone_weight "$clone_weight" \
  --arg status "$(status)" \
  '# The cold VM is the one weighed both times: the template VM, booted beside
   # it while the template was made, is gone by the second.
   ($cold_weight | map(.pid)) as $before
   | ($clone_weight | map(select(.pid as $pid | $before | index($pid))) | first) as $cold
   | ($clone_weight | map(select(.pid != $cold.pid) | .pss_kib)) as $pss
   | ($pss | add / length) as $clone_pss
   | {cold_page: $cold_page, cold_weight: $cold_weight, cold_pss_mib: ($cold.pss_kib / 1024 | floor),
      clone_pages: $clone_pages,
      weights: $clone_weight, clone_pss_mib: ($clone_pss / 1024 | floor),
      d4s_v5_clones: (12 * 1024 * 1024 / $clone_pss | floor),
      status: ($status | split("\n"))}' > "$out/report.json"
cat "$out/report.json"
