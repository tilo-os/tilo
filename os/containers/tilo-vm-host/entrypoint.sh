#!/bin/sh
# The VM host's start, as root in its privileged pod, then the service as
# tilo-hosted (10001):
#
#   0. the jail helper's capabilities (cap_setuid, cap_setgid), which only
#      root on the host itself can give: an image build's are its own;
#   1. user namespaces: each VM's network namespace is owned through one, which
#      Ubuntu's AppArmor refuses to unprivileged users unless told otherwise;
#   2. the cgroup tree: this pod's cgroup namespace is its root; the start moves
#      itself into a leaf so the root can hand cpu, memory and pids to the VMs'
#      cgroup (TILO_VM_CGROUP, the jail configuration's `cgroup`);
#   3. the state disk: a raw block device (TILO_VM_STATE_DEVICE) the pod's volume
#      follows from node to node; made XFS with reflink the first time, so every
#      VM's disk is a reflinked copy of the guest's root filesystem, and mounted
#      at /var/lib/tilo-hosted;
#   3b. the member identity: this host's own Tilo device, certified by the
#      operator that runs the relay, from TILO_VM_MEMBER_IDENTITIES (a
#      directory holding <pod name>.tar, made with tilo-enroll device-key and
#      install-cert), unpacked onto the state disk the first time only, so the
#      device key stays with the host's disk as the host moves between nodes:
#      the key and certificate under member/root (TILO_IDENTITY_ROOT), the
#      person's record under member/config/tilo/identity;
#   4. the guest: the image the release key signed, fetched once per digest
#      (TILO_VM_GUEST_REF overrides the one this build pins);
#   5. `tilo-legacy hosted serve`, jailed, which suspends every VM when the
#      eviction notice (TILO_VM_EVICT_NOTICE) appears and on SIGTERM, and wakes
#      no more VMs than its memory budget (TILO_VM_MEMORY_MIB, or auto from the
#      node's memory) holds, and answers a placer's Status on
#      TILO_VM_STATUS_LISTEN;
#   5b. with TILO_VM_RELAY_ENDPOINT (ADDR:PORT), each workspace's Browser in
#      its VM: the browser session there joins its workspace through the relay
#      at that address, the one address besides the internet a VM may reach;
#   6. with TILO_TAKE_WORK_RELAY, the member: `tilo-runtime --daemon`, as the
#      service, under the member identity, which takes the relay's runs for
#      the workspaces whose VMs are here (restarted if it stops).
set -eu

state_device=${TILO_VM_STATE_DEVICE:-/dev/tilo-vm-state}
cgroup=${TILO_VM_CGROUP:-tilo-vms}
data=/var/lib/tilo-hosted
run=/run/tilo-hosted

say() { printf 'tilo-vm-host: %s\n' "$*" >&2; }

# 0. the jail helper
setcap cap_setuid,cap_setgid+ep /usr/libexec/tilo/tilo-hosted-jail

# 1. user namespaces
if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
  echo 0 > /proc/sys/kernel/apparmor_restrict_unprivileged_userns
fi
[ "$(cat /proc/sys/user/max_user_namespaces)" -gt 0 ] || { say 'user namespaces are off'; exit 1; }

# 2. cgroups
grep -qw cgroup2 /proc/mounts || { say 'no cgroup v2'; exit 1; }
mkdir -p /sys/fs/cgroup/init
echo $$ > /sys/fs/cgroup/init/cgroup.procs
for controller in cpu memory pids; do
  echo "+$controller" > /sys/fs/cgroup/cgroup.subtree_control
done
mkdir -p "/sys/fs/cgroup/$cgroup"
for controller in cpu memory pids; do
  echo "+$controller" > "/sys/fs/cgroup/$cgroup/cgroup.subtree_control"
done

# 3. the state disk
[ -b "$state_device" ] || { say "no state disk at $state_device"; exit 1; }
if ! blkid -p "$state_device" >/dev/null 2>&1; then
  say "making XFS on the new state disk $state_device"
  # An XFS label holds 12 characters at most.
  mkfs.xfs -q -m reflink=1 -L tilovmstate "$state_device"
fi
mkdir -p "$data"
mountpoint -q "$data" || mount -t xfs -o noatime "$state_device" "$data"
mkdir -p "$data/state" "$data/images" "$data/jails"
chown tilo-hosted:tilo-hosted "$data" "$data/state" "$data/images"
chown root:root "$data/jails"
chmod 0755 "$data/jails"
mkdir -p "$run"
chown tilo-hosted:tilo-hosted "$run"

# 3b. the member identity
member=$data/member
pod=${HOSTNAME:-$(hostname)}
if [ -n "${TILO_VM_MEMBER_IDENTITIES:-}" ] && [ ! -e "$member/root/device-private/keypair.bin" ]; then
  [ -f "$TILO_VM_MEMBER_IDENTITIES/$pod.tar" ] || { say "no member identity for $pod"; exit 1; }
  say "installing the member identity of $pod"
  rm -rf "$member.new"
  mkdir -m 0700 "$member.new"
  tar -xf "$TILO_VM_MEMBER_IDENTITIES/$pod.tar" -C "$member.new" --no-same-owner
  chown -R tilo-hosted:tilo-hosted "$member.new"
  rm -rf "$member"
  mv "$member.new" "$member"
fi
# The person's record where an identity reads it: $XDG_CONFIG_HOME/tilo/identity.
if [ -d "$member/person" ] && [ ! -e "$member/config/tilo/identity" ]; then
  mkdir -p -m 0700 "$member/config/tilo"
  mv "$member/person" "$member/config/tilo/identity"
  chown -R tilo-hosted:tilo-hosted "$member/config"
fi

# 4. the guest
# The service's own home: skopeo reads its configuration under $HOME.
as_service() { HOME=$data setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all -- "$@"; }
if [ -n "${TILO_VM_GUEST_REF:-}" ]; then
  guest=$(as_service tilo-legacy hosted fetch-guest --images "$data/images" --ref "$TILO_VM_GUEST_REF")
else
  guest=$(as_service tilo-legacy hosted fetch-guest --images "$data/images")
fi
kernel=$(printf '%s\n' "$guest" | sed -n 's/^kernel //p')
rootfs=$(printf '%s\n' "$guest" | sed -n 's/^rootfs //p')
say "guest kernel $kernel, root filesystem $rootfs"

# 5. the service
set -- --state "$data/state" --run "$run" --kernel "$kernel" --disk-image "$rootfs" \
  --jail-helper /usr/libexec/tilo/tilo-hosted-jail --jail-config /etc/tilo/hosted-jail.json
if [ -n "${TILO_VM_PLANS:-}" ]; then set -- "$@" --plans "$TILO_VM_PLANS"; fi
if [ -n "${TILO_VM_EVICT_NOTICE:-}" ]; then set -- "$@" --evict-notice "$TILO_VM_EVICT_NOTICE"; fi
# auto: the node's memory less what the node and this service keep
# (TILO_VM_MEMORY_RESERVE_MIB, 3.5 GiB by default), in whole GiB: 12 GiB on a
# 16 GB machine, 59 GiB on a 64 GB one.
memory_mib=${TILO_VM_MEMORY_MIB:-}
if [ "$memory_mib" = auto ]; then
  total_mib=$(( $(sed -n 's/^MemTotal: *\([0-9]*\) kB/\1/p' /proc/meminfo) / 1024 ))
  memory_mib=$(( (total_mib - ${TILO_VM_MEMORY_RESERVE_MIB:-3584}) / 1024 * 1024 ))
  say "memory budget for VMs: $memory_mib MiB of the node's $total_mib"
fi
if [ -n "$memory_mib" ]; then set -- "$@" --memory-mib "$memory_mib"; fi
if [ -n "${TILO_VM_STATUS_LISTEN:-}" ]; then set -- "$@" --status-listen "$TILO_VM_STATUS_LISTEN"; fi
# The browser session in a workspace's VM joins its workspace through the
# relay at TILO_VM_RELAY_ENDPOINT, an address inside the host's network: it is
# the one destination besides the internet a VM may reach.
if [ -n "${TILO_VM_RELAY_ENDPOINT:-}" ]; then
  set -- "$@" --relay-endpoint "$TILO_VM_RELAY_ENDPOINT"
fi

# 6. the member
if [ -n "${TILO_TAKE_WORK_RELAY:-}" ]; then
  [ -e "$member/root/device-private/keypair.bin" ] || { say 'TILO_TAKE_WORK_RELAY is set, but this host has no member identity'; exit 1; }
  mkdir -p -m 0700 "$member/profile" "$member/home"
  chown tilo-hosted:tilo-hosted "$member/profile" "$member/home"
  chmod 0700 "$member/profile"
  (
    while :; do
      HOME=$member/home XDG_CONFIG_HOME=$member/config TILO_IDENTITY_ROOT=$member/root \
        TILO_RUNTIME_PROFILE=$member/profile TILO_HOSTED_VM_SOCKET=$run/hosted.sock \
        TILO_HOSTED_PLANS=${TILO_VM_PLANS:-} \
        setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all -- \
        tilo-runtime --daemon --profile "$member/profile" || say "the member stopped ($?); starting it again"
      sleep 5
    done
  ) &
fi
HOME=$data exec setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all -- tilo-legacy hosted serve "$@"
