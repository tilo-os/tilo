#!/bin/sh
# The VM host's start, as root in its privileged pod, then the service as
# tilo-hosted (10001):
#
#   0. the jail helper's capabilities (cap_setuid, cap_setgid), which only
#      root on the host itself can give: an image build's are its own;
#   1. user namespaces: each VM's network namespace is owned through one, which
#      Ubuntu's AppArmor refuses to unprivileged users unless told otherwise;
#   1b. /dev/net/tun open to every uid, as udev leaves it on a node: each VM's
#      network uid opens it for pasta's tap, and a pod's /dev made without
#      udev (devtmpfs's 0600) would refuse it, and every VM with it;
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
#      install-cert), unpacked onto the state disk when the disk holds no
#      identity or another one, so the device key stays with the host's disk as
#      the host moves between nodes. It goes where the member's profile reads
#      its identity (`tilo-runtime --profile` names its own TILO_IDENTITY_ROOT
#      and XDG_CONFIG_HOME under the profile): the key and certificate under
#      member/profile/identity, the person's record under
#      member/profile/config/tilo/identity. A profile made under another
#      identity is that device's, and is dropped with it;
#   4. the guest: the image the release key signed, fetched once per digest
#      (TILO_VM_GUEST_REF overrides the one this build pins);
#   5. `tilo-legacy hosted serve`, jailed, which suspends every VM when the
#      eviction notice (TILO_VM_EVICT_NOTICE) appears and on SIGTERM, admits
#      VMs by the memory they really take of the node's (TILO_VM_MEMORY_MIB,
#      or auto: all of it) less the reserve it keeps
#      (TILO_VM_MEMORY_RESERVE_MIB), and answers a placer's Status on
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

# 1b. the tun device
[ -c /dev/net/tun ] || { say 'no /dev/net/tun'; exit 1; }
chmod 0666 /dev/net/tun

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
profile=$member/profile
if [ -n "${TILO_VM_MEMBER_IDENTITIES:-}" ]; then
  identities=$TILO_VM_MEMBER_IDENTITIES/$pod.tar
  [ -f "$identities" ] || { say "no member identity for $pod"; exit 1; }
  installed=
  key=$profile/identity/device-private/keypair.bin
  if [ -f "$key" ]; then installed=$(sha256sum < "$key"); fi
  if [ "$(tar -xOf "$identities" root/device-private/keypair.bin | sha256sum)" != "$installed" ]; then
    say "installing the member identity of $pod"
    rm -rf "$member.new"
    mkdir -m 0700 "$member.new"
    tar -xf "$identities" -C "$member.new" --no-same-owner
    mkdir -m 0700 "$member.new/profile" "$member.new/profile/config" "$member.new/profile/config/tilo"
    mv "$member.new/root" "$member.new/profile/identity"
    mv "$member.new/person" "$member.new/profile/config/tilo/identity"
    chown -R tilo-hosted:tilo-hosted "$member.new"
    rm -rf "$member"
    mv "$member.new" "$member"
  fi
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
# The node's memory its VMs are packed into, and what stays free for the node
# itself. VMs are admitted by what they really use, which the service reads
# from each VM's processes, so the budget is the node's memory and not a sum
# of the sizes its VMs were configured at: auto takes all of it
# (TILO_VM_MEMORY_MIB). The reserve (TILO_VM_MEMORY_RESERVE_MIB, 2 GiB) is what
# the service keeps for this pod, its VMs' helper processes and the room a
# balloon needs to give memory back, and the headroom
# (TILO_VM_MEMORY_HEADROOM_MIB, 128 MiB) is added to what a VM is counted at
# until the kernel has measured it.
memory_mib=${TILO_VM_MEMORY_MIB:-}
if [ "$memory_mib" = auto ]; then
  total_mib=$(( $(sed -n 's/^MemTotal: *\([0-9]*\) kB/\1/p' /proc/meminfo) / 1024 ))
  memory_mib=$total_mib
  say "memory for VMs: all of the node's $memory_mib MiB"
fi
set -- "$@" --memory-reserve-mib "${TILO_VM_MEMORY_RESERVE_MIB:-2048}"
set -- "$@" --memory-headroom-mib "${TILO_VM_MEMORY_HEADROOM_MIB:-128}"
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
  [ -e "$profile/identity/device-private/keypair.bin" ] || { say 'TILO_TAKE_WORK_RELAY is set, but this host has no member identity'; exit 1; }
  mkdir -p -m 0700 "$profile" "$member/home"
  chown tilo-hosted:tilo-hosted "$profile" "$member/home"
  chmod 0700 "$profile"
  (
    while :; do
      # The profile names the member's identity and config itself.
      HOME=$member/home \
        TILO_RUNTIME_PROFILE=$profile TILO_HOSTED_VM_SOCKET=$run/hosted.sock \
        TILO_HOSTED_PLANS=${TILO_VM_PLANS:-} \
        setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all -- \
        tilo-runtime --daemon --profile "$profile" || say "the member stopped ($?); starting it again"
      sleep 5
    done
  ) &
fi
HOME=$data exec setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all -- tilo-legacy hosted serve "$@"
