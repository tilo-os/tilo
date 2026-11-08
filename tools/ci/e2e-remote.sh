#!/usr/bin/env bash
# An agent on one of the person's Tilo boxes steering another of their boxes over MCP, end to
# end on real VMs (scripts/e2e-remote/run.py): this tree's dev disk (os-dev-disk-x86_64) booted
# by tilo-vm (tilo-vm-binary-x86_64) on libvirt, in a micro-VM with KVM.
#
# Box A is set up once (its person made by onboarding) and branched into box B with
# `tilo-vm vm branch`, so the two are two certified devices of one person; box C is another
# person's. The three share one local network and nothing else: a bridge here, each guest on
# it through a tap device of its own, with no DHCP, no DNS and no route out, so the boxes find
# each other the way two devices on a home network without the internet do.
#
#   e2e-remote.sh           what the e2e-remote check runs
#
# `t test e2e-remote --test hold` keeps the micro-VM and its guests up for an hour after the
# run, whatever its outcome, for `t vm ssh` (and `t vm extend`).
set -euo pipefail

arch=x86_64
work=/var/tmp/e2e-remote
out=${CARGO_TARGET_DIR:-/target}/e2e-remote
gate=/target/publish/os-dev-disk/$arch/disk
tilo_vm=/target/tilo-vm-bin/tilo-vm
run_user=acceptance
bridge=tiloe2e0
taps=(tilo-e2e-a tilo-e2e-b tilo-e2e-c)
phase() { echo "e2e-remote: $1 at ${SECONDS}s"; }

bash tools/ci/install-packages.sh tools/ci/packages/vm-acceptance.txt tools/ci/packages/e2e-remote.txt
command -v virsh qemu-system-x86_64 qemu-img passt ssh sudo ip >/dev/null
phase 'tools ready'
# Three boxes of run.py's MEMORY_MB each run here: what this micro-VM has for them, for the log.
echo "e2e-remote: $(nproc) CPUs, $(awk '/MemTotal|MemAvailable/ {printf "%s %d MiB ", $1, $2/1024}' /proc/meminfo)"

./os/dev/assert-native-arch.sh "$arch"
python3 os/dev/probe-kvm.py "$arch" || {
  echo 'this node cannot create a KVM VM; the boxes never run under TCG' >&2
  exit 1
}

rm -rf "$work" "$out"
mkdir -p "$work" "$out"

disk=$gate/disk.qcow2
[[ -s $disk && -s $gate/image-digest ]] \
  || { echo "no installed dev disk in $gate: the check needs os-dev-disk-$arch" >&2; exit 1; }
[[ -x $tilo_vm ]] || { echo "no $tilo_vm: the check needs tilo-vm-binary-$arch" >&2; exit 1; }
install -m 0755 "$tilo_vm" /usr/local/bin/tilo-vm
commit=${TILO_ACCEPTANCE_COMMIT:-$(git rev-parse HEAD)}
echo "boxes from ${commit:0:12}'s dev image $(cat "$gate/image-digest")"

# The installed disk is never booted: A's disk and C's are qcow2 overlays of it that the session
# user owns (B's is A's branch).
for box in a c; do
  qemu-img create -q -f qcow2 -F qcow2 -b "$disk" "$work/box-$box.qcow2"
done

# libvirt's session daemon runs as an unprivileged user, as on a workstation (vm-acceptance.sh).
id "$run_user" >/dev/null 2>&1 || useradd --create-home --shell /bin/bash "$run_user"
uid=$(id -u "$run_user")
install -d -m 0700 -o "$run_user" -g "$run_user" "/run/user/$uid"
[[ -w /dev/kvm && -r /dev/kvm ]] && [[ $(stat -c %a /dev/kvm) == 666 ]] || chmod 0666 /dev/kvm
install -d -o "$run_user" -g "$run_user" "/home/$run_user/.config" "/home/$run_user/.config/libvirt"
printf 'max_core = 0\n' > "/home/$run_user/.config/libvirt/qemu.conf"
chown "$run_user:" "/home/$run_user/.config/libvirt/qemu.conf"

# The boxes' own network: a bridge with a tap per box that the session user may open, which
# libvirt hands its guest as an unmanaged ethernet interface. Multicast floods the bridge, so
# what one box announces on the local network the others hear.
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

chown -R "$run_user:" "$work" "$out"
keep=$(env | sed -n 's/^\(TILO_[A-Z0-9_]*\|T_[A-Z0-9_]*\)=.*/\1/p' | paste -sd, -)
as_user() { sudo -n -u "$run_user" -H --preserve-env="$keep" env "XDG_RUNTIME_DIR=/run/user/$uid" "PATH=$PATH" "$@"; }
caps=$(as_user virsh --connect qemu:///session capabilities)
grep -q "<domain type='kvm'" <<<"$caps" || { echo "libvirt's session daemon offers no KVM domain" >&2; exit 1; }
phase 'host ready'

args=(--work "$work" --out "$out" --commit "$commit" --tilo-vm /usr/local/bin/tilo-vm
      --taps "$(IFS=,; echo "${taps[*]}")")
[[ ${T_TEST_FILTER:-} != *hold* ]] || args+=(--hold 3600)
status=0
as_user env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*' \
  python3 scripts/e2e-remote/run.py "${args[@]}" || status=$?
[[ ! -f $out/report.md ]] || cat "$out/report.md"
phase 'done'
exit "$status"
