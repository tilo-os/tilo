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
#   3. the member identity: this host's own Tilo device, its one key and the
#      certificates people gave it under member/identity on the state disk, so
#      the device key stays with the host's disk as the host moves between
#      nodes. The ticket an owner device printed for it (TILO_VM_TICKET) is
#      handed to the member's daemon as its `tilo.ticket` credential, which it
#      spends at its first start and leaves alone once a certificate names its
#      key. A host given no ticket and with no certificate fails, unless
#      TILO_VM_FOUND=1: then, once the member's daemon runs, it asks it to
#      found the host's own identity, as a machine with no owner before it
#      does (`tilo login --new-identity`).
#   4. the member: `tilo-runtime`, as the service, under the member
#      identity, its endpoint on TILO_DEVICE_PORT (7703 where none is named),
#      an ordinary device of the workspaces it serves that takes their Linux
#      turns when it has room or keeps their VM (the login names the server),
#      restarted if it stops;
#   5. tilo-hosted-serve, the start this image and the server OS share: the
#      guest (the image the release key signed, fetched once per digest;
#      TILO_VM_GUEST_REF overrides the one this build pins) and then
#      `tilo-linux-vm hosted serve`, jailed, which suspends every VM when the
#      eviction notice (TILO_VM_EVICT_NOTICE) appears and on SIGTERM, admits
#      VMs by the memory they really take of the node's (TILO_VM_MEMORY_MIB,
#      or auto: all of it) less the reserve it keeps
#      (TILO_VM_MEMORY_RESERVE_MIB). With TILO_VM_SERVER_ENDPOINT (ADDR:PORT), each
#      workspace's Browser in its VM joins its workspace through the server at
#      that address, the one address besides the internet a VM may reach.
set -eu

state_device=${TILO_VM_STATE_DEVICE:-/dev/tilo-vm-state}
data=/var/lib/tilo-hosted
run=/run/tilo-hosted

say() { printf 'tilo-vm-host: %s\n' "$*" >&2; }

# 0. the jail helper
setcap cap_setuid,cap_setgid+ep /usr/libexec/tilo/tilo-hosted-jail

# 0b. the run directory, where the service's socket and each VM's sockets are,
# on a tmpfs of its own as on the server OS: a set-group-id directory there gives
# every socket Firecracker makes the service's group. A container's own root is an
# overlay, and through FUSE (a rootful podman on some hosts) it gives them
# Firecracker's group, so the service could not reach any VM's agent.
mkdir -p "$run"
mountpoint -q "$run" || mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs "$run" \
  || { say "could not mount a tmpfs at $run"; exit 1; }

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
profile=$member/profile
identity=$member/identity
credentials=$member/credentials
mkdir -p -m 0700 "$member" "$member/home" "$profile" "$identity" "$credentials"
chown tilo-hosted:tilo-hosted "$member" "$member/home" "$profile" "$identity" "$credentials"
chmod 0700 "$profile"
# A host started before kept its identity in the profile.
if [ -d "$profile/identity" ] && [ ! -e "$identity/device-private" ]; then
  cp -a "$profile/identity/." "$identity/" && rm -rf "$profile/identity"
fi
certified() {
  [ -s "$identity/device/cert.bin" ] || ls "$identity/device/certs/"*.bin >/dev/null 2>&1
}
rm -f "$credentials/tilo.ticket"
if certified; then
  : # this host is one of the person's devices already; a ticket beside it is left unspent
elif [ -n "${TILO_VM_TICKET:-}" ]; then
  say "joining the person's devices as ${HOSTNAME:-$(hostname)}"
  printf '%s\n' "$TILO_VM_TICKET" > "$credentials/tilo.ticket"
  chown tilo-hosted:tilo-hosted "$credentials/tilo.ticket"
  chmod 0600 "$credentials/tilo.ticket"
elif [ "${TILO_VM_FOUND:-0}" = 1 ]; then
  # The member's daemon makes the person (step 4), as it makes every person.
  found_here=1
else
  say "this host has no identity and no TILO_VM_TICKET: a stack.sh up --fresh makes a person whose founding issues the tickets, or take one an owner's device issued"
  exit 1
fi

# 4. the member
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
    # The profile names the member's config; its identity is the host's.
    HOME=$home TILO_IDENTITY_ROOT=$identity CREDENTIALS_DIRECTORY=$credentials \
      TILO_RUNTIME_PROFILE=$profile TILO_HOSTED_VM_SOCKET=$run/hosted.sock \
      TILO_HOSTED_PLANS=${TILO_VM_PLANS:-} \
      setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all -- \
      tilo-runtime --profile "$profile" || say "the member stopped ($?); starting it again"
    sleep 5
  done
) &

# A host that founds its own identity asks its member's daemon to, once the daemon answers: a
# person is made by the daemon alone.
if [ "${found_here:-0}" = 1 ]; then
  say "this host founds its own identity, as a machine with no owner before it does"
  for _ in $(seq 1 120); do
    [ -S "$profile/native.sock" ] && break
    sleep 1
  done
  HOME=$home TILO_IDENTITY_ROOT=$identity TILO_RUNTIME_PROFILE=$profile \
    XDG_CONFIG_HOME=$profile/config XDG_DATA_HOME=$profile/data \
    XDG_CACHE_HOME=$profile/cache XDG_STATE_HOME=$profile/state \
    setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all -- \
    tilo login --new-identity >/dev/null \
    || { say "this host could not found its own identity"; exit 1; }
fi

# 5. the guest and the service: tilo-hosted-serve, the start the server OS runs
# too (it fetches the pinned guest, then serves the VMs as the service's own
# account).
exec tilo-hosted-serve --state "$data/state" --run "$run"
