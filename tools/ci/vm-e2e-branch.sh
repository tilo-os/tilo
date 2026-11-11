#!/usr/bin/env bash
# Branching a configured box, end to end, in real VMs (scripts/vm-e2e/branch.py): this tree's
# own dev disk (os-dev-disk-x86_64) booted with this tree's tilo-vm (tilo-vm-binary-x86_64) on
# libvirt, in a micro-VM on an x86-64 node with KVM, as vm-acceptance boots it.
#
#   vm-e2e-branch.sh        what e2e-branch runs
#
# box, a and b share one local network beside each one's own way out (passt): a bridge here,
# each guest on it through a tap of its own, as a person's machines on one home network.
#
# The report (report.md, report.json) and each VM's evidence land in /target/e2e-branch.
# `t test e2e-branch --test hold` (or `t dispatch e2e-branch --input hold=SECONDS`) keeps the
# micro-VM and its guests up after the steps, for `t vm ssh`.
set -euo pipefail

arch=x86_64
work=/var/tmp/e2e-branch
reports=/target/e2e-branch
gate=/target/publish/os-dev-disk/$arch/disk
tilo_vm=/target/tilo-vm-bin/tilo-vm
run_user=acceptance
bridge=tilobranch0
taps=(tilo-br-box tilo-br-a tilo-br-b)
phase() { echo "e2e-branch: $1 at ${SECONDS}s"; }

bash tools/ci/install-packages.sh tools/ci/packages/vm-acceptance.txt tools/ci/packages/e2e-remote.txt
command -v virsh qemu-system-x86_64 qemu-img passt ssh sudo ip >/dev/null
echo "e2e-branch: $(nproc) CPUs, $(awk '/MemTotal|MemAvailable/ {printf "%s %d MiB ", $1, $2/1024}' /proc/meminfo)"
./os/dev/assert-native-arch.sh "$arch"
python3 os/dev/probe-kvm.py "$arch" || {
  echo 'this node cannot create a KVM VM; the branch check never runs under TCG' >&2
  exit 1
}

rm -rf "$work" "$reports"
mkdir -p "$work" "$reports"
disk=$gate/disk.qcow2
[[ -s $disk && -s $gate/image-digest ]] \
  || { echo "no installed dev disk in $gate: the check needs os-dev-disk-$arch" >&2; exit 1; }
[[ -x $tilo_vm ]] || { echo "no $tilo_vm: the check needs tilo-vm-binary-$arch" >&2; exit 1; }
commit=${TILO_ACCEPTANCE_COMMIT:-$(git rev-parse HEAD)}
install -m 0755 "$tilo_vm" /usr/local/bin/tilo-vm
echo "branching a box of ${commit:0:12}'s dev image $(cat "$gate/image-digest")"

# The installed disk is never booted: box's disk is a qcow2 overlay of it the session user owns.
base="$work/tilo-dev-${commit:0:9}.qcow2"
qemu-img create -q -f qcow2 -F qcow2 -b "$disk" "$base"

# libvirt's session daemon runs as an unprivileged user, as on a workstation.
id "$run_user" >/dev/null 2>&1 || useradd --create-home --shell /bin/bash "$run_user"
uid=$(id -u "$run_user")
install -d -m 0700 -o "$run_user" -g "$run_user" "/run/user/$uid"
[[ -w /dev/kvm && -r /dev/kvm ]] && [[ $(stat -c %a /dev/kvm) == 666 ]] || chmod 0666 /dev/kvm
install -d -o "$run_user" -g "$run_user" "/home/$run_user/.config" "/home/$run_user/.config/libvirt"
printf 'max_core = 0\n' > "/home/$run_user/.config/libvirt/qemu.conf"
chown "$run_user:" "/home/$run_user/.config/libvirt/qemu.conf"
mkdir -p target/dev-runs
chown -R "$run_user:" "$work" "$reports" target

# The boxes' local network: a bridge with a tap per box the session user may open, which
# libvirt hands its guest as an unmanaged ethernet interface (scripts/vm-e2e/branch.py).
if [[ ! -c /dev/net/tun ]]; then
  modprobe tun 2>/dev/null || true
  [[ -c /dev/net/tun ]] || { mkdir -p /dev/net; mknod -m 0666 /dev/net/tun c 10 200; }
fi
chmod 0666 /dev/net/tun
ip link del "$bridge" 2>/dev/null || true
ip link add "$bridge" type bridge
echo 0 > "/sys/class/net/$bridge/bridge/multicast_snooping"
ip link set "$bridge" up
for tap in "${taps[@]}"; do
  ip link del "$tap" 2>/dev/null || true
  ip tuntap add dev "$tap" mode tap user "$run_user"
  ip link set "$tap" master "$bridge" up
done
as_user() { sudo -n -u "$run_user" -H env "XDG_RUNTIME_DIR=/run/user/$uid" "PATH=$PATH" "$@"; }
caps=$(as_user virsh --connect qemu:///session capabilities)
grep -q "<domain type='kvm'" <<<"$caps" || { echo "libvirt's session daemon offers no KVM domain" >&2; exit 1; }
phase 'VM host ready'

hold=${T_INPUT_HOLD:-0}
[[ ${T_TEST_FILTER:-} != *hold* ]] || hold=3600
# `t test e2e-branch --test boot` stops after the boxes' first boot and its checks.
extra=()
[[ ${T_TEST_FILTER:-} != *boot* ]] || extra+=(--boot-only)
status=0
as_user env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*' \
  python3 scripts/vm-e2e/branch.py --disk "$base" --commit "$commit" --tilo-vm /usr/local/bin/tilo-vm \
  --out "$reports" --taps "$(IFS=,; echo "${taps[*]}")" --hold "$hold" "${extra[@]}" || status=$?
[[ ! -f $reports/report.md ]] || cat "$reports/report.md"
# A failed check keeps no outputs: its evidence (each box's boot journal of its device identity,
# the person's session where a step could not reach it) is printed here.
if (( status != 0 )); then
  for f in "$reports"/data/*-boot.txt "$reports"/data/*-session.txt "$reports"/data/*-session.log "$reports"/data/S03-box-person.txt "$reports"/data/*-actions.txt; do
    [[ -f $f ]] || continue
    echo "== evidence: ${f##*/}"
    { grep -av 'FRAME-LIFECYCLE\|paint-stats' "$f" | tail -n 450 | cut -c1-1500; } || true
  done
fi
phase 'done'
exit "$status"
