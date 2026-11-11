#!/usr/bin/env bash
# The local tilo.sh stack's VM host, brought up the way stack.sh brings it up
# (os/containers/tilo-sh/vm-host.sh) and held to what a hosted workspace needs
# under it: the placer places the workspace on vm-host-0, and vm-host-0 wakes
# that workspace's VM in its jail from the guest image this checkout built and
# runs a program in it.
#
# usage: test-vm-host.sh VM_HOST_ARCHIVE HOSTED_GUEST_DIR ENROLL
#
# VM_HOST_ARCHIVE is image-vm-host's image.oci.tar, HOSTED_GUEST_DIR holds the
# image-hosted-guest check's image.oci.tar with the digest beside it, and
# ENROLL is a tilo-enroll.
#
# What it needs: root, /dev/kvm and a loop device, in a micro-VM of its own (a
# t check with `isolation = "vm"` and `requires = ["/dev/kvm"]`): a VM host
# runs privileged containers, loop devices and Firecracker.
set -euo pipefail
archive=${1:?usage: test-vm-host.sh VM_HOST_ARCHIVE HOSTED_GUEST_DIR ENROLL}
guest=${2:?the guest image outputs}
enroll=${3:?the tilo-enroll program}

say() { printf 'tilo-sh vm-host check: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }

[ "$(id -u)" = 0 ] || die "a VM host runs root containers: this runs as $(id -u)"
[ -e /dev/kvm ] || die '/dev/kvm is not there: this host cannot run the VMs a hosted workspace needs'
command -v podman >/dev/null || die 'no podman: this check needs the container runtime the stack runs'
command -v losetup >/dev/null || die 'no losetup: the state disk is a loop device'
python3 os/dev/probe-kvm.py "$(uname -m)" \
  || die 'this machine offers /dev/kvm but cannot create a VM with it: a VM host needs nested virtualization'
[ -x "$enroll" ] || command -v "$enroll" >/dev/null || die "$enroll is not a tilo-enroll"

state=$(mktemp -d /var/lib/tilo-sh-check.XXXXXX)
cleanup() {
  TILO_SH_STATE=$state bash "$(dirname -- "$0")/vm-host.sh" down >/dev/null 2>&1 || true
  rm -rf "$state"
}
trap cleanup EXIT

# The relay this stack runs places on it: the host's member device is
# certified by the relay's owner root, and reaches the relay at its own
# device. The stack's relay leaves both; this check starts no relay.
mkdir -p "$state/secrets" "$state/rooms/lounge/relay"
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$state/secrets/room-root-lounge"
(umask 077 && head -c 32 /dev/urandom > "$state/rooms/lounge/relay/host-device.key")

say "loading $archive"
podman load -i "$archive" > "$state/load.log" 2>&1 || { cat "$state/load.log" >&2; die "podman load $archive"; }
image=$(sed -n 's/^Loaded image[s]*: //p' "$state/load.log" | tail -n 1)
[ -n "$image" ] || { cat "$state/load.log" >&2; die "podman load named no image"; }

vm_host() {
  TILO_SH_STATE=$state \
  TILO_SH_IMAGE_VM_HOST=$image \
  TILO_SH_HOSTED_GUEST=$guest \
  TILO_SH_TILO_ENROLL=$enroll \
    bash "$(dirname -- "$0")/vm-host.sh" "$@"
}

vm_host up
say 'the host is up'
podman logs tilo-sh-vm-host-0 > "$state/host.log" 2>&1 || true
sed -n '1,30p' "$state/host.log" >&2

# What the host and its VMs said when one would not run: the console log is
# where Firecracker and the jail helper write why they stopped.
vm_failure() {
  say "$1"
  podman logs --tail 100 tilo-sh-vm-host-0 >&2 || true
  podman exec tilo-sh-vm-host-0 sh -c 'for log in /var/lib/tilo-hosted/state/vms/*/*/console.log; do
      [ -f "$log" ] || continue
      echo "== $log"
      cat "$log"
    done' >&2 || true
  podman exec tilo-sh-vm-host-0 sh -c 'ls -la /var/lib/tilo-hosted /var/lib/tilo-hosted/jails 2>&1
    ls -la /sys/fs/cgroup 2>&1 | head -n 20
    cat /sys/fs/cgroup/cgroup.controllers 2>&1' >&2 || true
  die "$1"
}

# A workspace's VM wakes from the guest this checkout built and runs a program
# in it: Firecracker boots it in its jail, its agent answers, and the program's
# output comes back.
ran=$(podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 --init-groups \
  --inh-caps=-all tilo-legacy hosted run --run /run/tilo-hosted \
  --workspace ws-local --account acct-with-card --size small -- /bin/sh -c 'echo hello-from-the-guest' 2>&1) \
  || vm_failure 'the workspace VM did not run a program'
printf '%s\n' "$ran" >&2
grep -qx 'hello-from-the-guest' <<<"$ran" \
  || die 'the program in the workspace VM printed something else'

# The host says which VMs it holds, and the placer which machine runs them.
held=$(vm_host status)
printf '%s\n' "$held" >&2
grep -q 'ws-local *small *Running' <<<"$held" || die 'the host does not hold the workspace VM it woke'
podman logs tilo-sh-vm-placer > "$state/placer.log" 2>&1 || true
sed -n '1,10p' "$state/placer.log" >&2
grep -q 'placing on vm-host-0, at most 1' "$state/placer.log" \
  || die 'the placer does not place on vm-host-0'

# The guest the host boots is the image this check handed it, unpacked under
# its digest, and its member device is the one the relay's root certified.
grep -q "guest kernel .*hosted-$(tr -d '[:space:]' < "$guest/digest" | sed 's/^sha256://')" \
  "$state/host.log" || die 'the guest is not the local image this check handed it'
grep -q 'installing the member identity of vm-host-0' "$state/host.log" \
  || die 'the host installed no member identity'

vm_host down
[ ! -d "$state/vm" ] || die 'down left the state disk behind'
say 'a workspace VM ran on the local stack'"'"'s VM host, and it went down clean'
