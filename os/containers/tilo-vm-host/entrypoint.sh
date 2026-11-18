#!/bin/sh
# The VM host's start, as root in its privileged pod, then the service as
# tilo-hosted (10001):
#
#   0. the jail helper's capabilities (cap_setuid, cap_setgid), which only
#      root on the host itself can give: an image build's are its own (the
#      server OS gives them to the service instead, tilo-hosted.service);
#   1. the cgroup root: this pod's cgroup namespace is its root; the start moves
#      itself into a leaf so the root can hand cpu, memory and pids to the VMs'
#      cgroup (tilo-hosted-prepare, below);
#   2. the state disk: a raw block device (TILO_VM_STATE_DEVICE) the pod's volume
#      follows from node to node; made XFS with reflink the first time, so every
#      VM's disk is a reflinked copy of the guest's root filesystem, and mounted
#      at /var/lib/tilo-hosted;
#   2b. tilo-hosted-prepare, the machine setup the server OS runs too: user
#      namespaces (each VM's network namespace is owned through one, which
#      Ubuntu's AppArmor refuses to unprivileged users unless told otherwise),
#      /dev/net/tun open to every uid (each VM's network uid opens it for pasta's
#      tap, and a pod's /dev made without udev would refuse it), the VMs'
#      cgroup, and the service's directories;
#   3. the member identity: this host's own Tilo device, certified by the
#      operator that runs the server, from TILO_VM_MEMBER_IDENTITIES (a
#      operator that runs the server, from TILO_VM_MEMBER_IDENTITIES (a
#      directory holding <pod name>.tar, made with tilo-enroll device-key and
#      install-cert), unpacked onto the state disk when the disk holds no
#      identity or another one, so the device key stays with the host's disk as
#      the host moves between nodes. It goes where the member's profile reads
#      its identity (`tilo-runtime --profile` names its own TILO_IDENTITY_ROOT
#      and XDG_CONFIG_HOME under the profile): the key and certificate under
#      member/profile/identity, the person's record under
#      member/profile/config/tilo/identity. A profile made under another
#      identity is that device's, and is dropped with it;
#   4. with TILO_TAKE_WORK_SERVER, the member: `tilo-runtime --daemon`, as the
#      service, under the member identity, which takes the server's runs for
#      the workspaces whose VMs are here (restarted if it stops);
#   5. tilo-hosted-serve, the start this image and the server OS share: the
#      guest (the image the release key signed, fetched once per digest;
#      TILO_VM_GUEST_REF overrides the one this build pins) and then
#      `tilo-linux-vm hosted serve`, jailed, which suspends every VM when the
#      eviction notice (TILO_VM_EVICT_NOTICE) appears and on SIGTERM, admits
#      VMs by the memory they really take of the node's (TILO_VM_MEMORY_MIB,
#      or auto: all of it) less the reserve it keeps
#      (TILO_VM_MEMORY_RESERVE_MIB), and answers a placer's Status on
#      TILO_VM_STATUS_LISTEN. With TILO_VM_SERVER_ENDPOINT (ADDR:PORT), each
#      workspace's Browser in its VM joins its workspace through the server at
#      that address, the one address besides the internet a VM may reach.
set -eu

state_device=${TILO_VM_STATE_DEVICE:-/dev/tilo-vm-state}
data=/var/lib/tilo-hosted
run=/run/tilo-hosted

say() { printf 'tilo-vm-host: %s\n' "$*" >&2; }

# 0. the jail helper
setcap cap_setuid,cap_setgid+ep /usr/libexec/tilo/tilo-hosted-jail

# 1. cgroups: this pod's cgroup namespace is its root, and a cgroup that runs a
# process hands its controllers to no children. The start moves itself into a
# leaf, and tilo-hosted-prepare (step 2) hands the controllers to the VMs' cgroup
# from the root it now owns.
grep -qw cgroup2 /proc/mounts || { say 'no cgroup v2'; exit 1; }
mkdir -p /sys/fs/cgroup/init
echo $$ > /sys/fs/cgroup/init/cgroup.procs

# 2. the state disk
[ -b "$state_device" ] || { say "no state disk at $state_device"; exit 1; }
if ! blkid -p "$state_device" >/dev/null 2>&1; then
  say "making XFS on the new state disk $state_device"
  # An XFS label holds 12 characters at most.
  mkfs.xfs -q -m reflink=1 -L tilovmstate "$state_device"
fi
mkdir -p "$data"
mountpoint -q "$data" || mount -t xfs -o noatime "$state_device" "$data"

# 2b. what hosting needs on any machine, as root: user namespaces, the tun
# device, the cgroup tree, and the service's directories.
tilo-hosted-prepare

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

# 4. the member
if [ -n "${TILO_TAKE_WORK_SERVER:-}" ]; then
  [ -e "$profile/identity/device-private/keypair.bin" ] || { say 'TILO_TAKE_WORK_SERVER is set, but this host has no member identity'; exit 1; }
  mkdir -p -m 0700 "$profile" "$member/home"
  chown tilo-hosted:tilo-hosted "$profile" "$member/home"
  chmod 0700 "$profile"
  # The member's home, where it keeps its workspaces' files, is on the state
  # disk, under /var/lib. A workspace's VM sees a folder shared with it at the
  # path it has here, and /var/lib is the guest's own, so no folder under it is
  # shared: the member reaches its home at /home/tilo-member instead.
  home=/home/tilo-member
  mkdir -p "$home"
  mountpoint -q "$home" || mount --bind "$member/home" "$home" \
    || { say "could not mount the member's home at $home"; exit 1; }
  (
    while :; do
      # The profile names the member's identity and config itself.
      HOME=$home \
        TILO_RUNTIME_PROFILE=$profile TILO_HOSTED_VM_SOCKET=$run/hosted.sock \
        TILO_HOSTED_PLANS=${TILO_VM_PLANS:-} \
        setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all -- \
        tilo-runtime --daemon --profile "$profile" || say "the member stopped ($?); starting it again"
      sleep 5
    done
  ) &
fi

# 5. the guest and the service: tilo-hosted-serve, the start the server OS runs
# too (it fetches the pinned guest, then serves the VMs as the service's own
# account).
exec tilo-hosted-serve --state "$data/state" --run "$run"
