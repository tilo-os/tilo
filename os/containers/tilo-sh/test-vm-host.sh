#!/usr/bin/env bash
# The local tilo.sh stack's VM host, brought up the way stack.sh brings it up
# (os/containers/tilo-sh/vm-host.sh) and held to what a hosted workspace needs
# under it: vm-host-0 wakes a workspace's VM in its jail from the guest image this checkout built and
# runs a program in it. That machine is then branched, running and suspended:
# each branch holds what the tenant installed and wrote and none of the
# source's sessions, and is deleted, its disk gone, while the source runs on.
#
# usage: test-vm-host.sh VM_HOST_ARCHIVE HOSTED_GUEST_DIR
#
# VM_HOST_ARCHIVE is image-vm-host's image.oci.tar, HOSTED_GUEST_DIR holds the
# image-hosted-guest check's image.oci.tar with the digest beside it. The host
# founds its own identity here: this check starts no server, so there is no
# person to join (TILO_VM_FOUND).
#
# What it needs: root, /dev/kvm and a loop device, in a micro-VM of its own (a
# t check with `isolation = "vm"` and `requires = ["/dev/kvm"]`): a VM host
# runs privileged containers, loop devices and Firecracker.
set -euo pipefail
archive=${1:?usage: test-vm-host.sh VM_HOST_ARCHIVE HOSTED_GUEST_DIR}
guest=${2:?the guest image outputs}

say() { printf 'tilo-sh vm-host check: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }

[ "$(id -u)" = 0 ] || die "a VM host runs root containers: this runs as $(id -u)"
[ -e /dev/kvm ] || die '/dev/kvm is not there: this host cannot run the VMs a hosted workspace needs'
command -v podman >/dev/null || die 'no podman: this check needs the container runtime the stack runs'
command -v losetup >/dev/null || die 'no losetup: the state disk is a loop device'
python3 os/dev/probe-kvm.py "$(uname -m)" \
  || die 'this machine offers /dev/kvm but cannot create a VM with it: a VM host needs nested virtualization'

state=$(mktemp -d /var/lib/tilo-sh-check.XXXXXX)
cleanup() {
  TILO_SH_STATE=$state bash "$(dirname -- "$0")/vm-host.sh" down >/dev/null 2>&1 || true
  rm -rf "$state"
}
trap cleanup EXIT

# The server this stack runs places on it: the host's member device reaches the
# server at its own device. This check starts no server, and gives the host no
# ticket, so it founds its own identity; the stack's server leaves both.
mkdir -p "$state/secrets" "$state/workspaces/lounge/server"
(umask 077 && head -c 32 /dev/urandom > "$state/workspaces/lounge/server/host-device.key")

say "loading $archive"
podman load -i "$archive" > "$state/load.log" 2>&1 || { cat "$state/load.log" >&2; die "podman load $archive"; }
image=$(sed -n 's/^Loaded image[s]*: //p' "$state/load.log" | tail -n 1)
[ -n "$image" ] || { cat "$state/load.log" >&2; die "podman load named no image"; }

vm_host() {
  TILO_SH_STATE=$state \
  TILO_SH_IMAGE_VM_HOST=$image \
  TILO_SH_HOSTED_GUEST=$guest \
  TILO_VM_FOUND=1 \
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
  --inh-caps=-all tilo-linux-vm hosted run --run /run/tilo-hosted \
  --workspace ws-local --account acct-tilo-sh --size small -- /bin/sh -c 'echo hello-from-the-guest' 2>&1) \
  || vm_failure 'the workspace VM did not run a program'
printf '%s\n' "$ran" >&2
grep -qx 'hello-from-the-guest' <<<"$ran" \
  || die 'the program in the workspace VM printed something else'

# The host says which VMs it holds.
held=$(vm_host status)
printf '%s\n' "$held" >&2
grep -q '^ws-local/default  *small  *Running' <<<"$held" || die 'the host does not hold the workspace VM it woke'

# The guest the host boots is the image this check handed it, unpacked under
# its digest, and its member device is the one the server's root certified. The
# host's log as it is now, by when the host has served a VM: the copy taken
# right after `up` may hold only its first line.
podman logs tilo-sh-vm-host-0 > "$state/host.log" 2>&1 || true
grep -q "guest kernel .*hosted-$(tr -d '[:space:]' < "$guest/digest" | sed 's/^sha256://')" \
  "$state/host.log" || {
  # What it judged by.
  say "looked for: guest kernel .*hosted-$(tr -d '[:space:]' < "$guest/digest" | sed 's/^sha256://')"
  say "host.log's guest kernel lines: $(grep 'guest kernel' "$state/host.log" || echo none)"
  say "the host's log now: $(podman logs tilo-sh-vm-host-0 2>&1 | grep 'guest kernel' || echo none)"
  die 'the guest is not the local image this check handed it'
}
member=$(podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 --init-groups \
  --inh-caps=-all -- env HOME=/var/lib/tilo-hosted/member/home \
  XDG_STATE_HOME=/var/lib/tilo-hosted/member/profile/state \
  TILO_IDENTITY_ROOT=/var/lib/tilo-hosted/member/identity \
  tilo device key 2>&1)
printf '%s\n' "$member" >&2
grep -q '"device"' <<<"$member" || die 'the host holds no member device key'

# A program in a workspace's VM as the tenant, and a question to the host, as
# an operator asks them.
as_service() {
  podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 --init-groups \
    --inh-caps=-all tilo-linux-vm hosted "$@"
}
in_vm() { # workspace script
  as_service run --run /run/tilo-hosted --workspace "$1" --account acct-tilo-sh --size small \
    -- /bin/sh -c "$2" 2>&1
}
hosted() { as_service "$@" --run /run/tilo-hosted; }
phase_of() { hosted status | awk -v vm="$1/default" '$1 == vm { print $3 }'; }
vm_dir() { printf '/var/lib/tilo-hosted/state/vms/%s' "$1"; }

# A machine set up once and branched per task: what the tenant installed and
# wrote in the source is in each branch, the source's sessions are not, and
# each machine's later writes are its own. The branch's disk is all that is
# copied, so it boots cold; the source runs on as it was.
say 'setting up the source machine'
# Each line is echoed as it runs (set -x), so a setup that stops names the line.
setup=$(in_vm ws-local 'set -ex
  echo written-in-the-source > "$HOME/branch-proof"
  printf "[user]\n\tname = Branch Proof\n" > "$HOME/.gitconfig"
  mkdir -p "$HOME/.ssh"
  echo "Host branch-proof" > "$HOME/.ssh/config"
  printf "#!/bin/sh\necho proof-tool-ran\n" > /tmp/proof-tool
  sudo -n install -m 0755 /tmp/proof-tool /usr/local/bin/proof-tool
  mkdir -p "$HOME/.config/tilo/identity/device" "$HOME/.local/share/tilo/secrets"
  echo not-a-real-key > "$HOME/.config/tilo/identity/device/keypair.bin"
  echo not-a-real-secret > "$HOME/.local/share/tilo/secrets/workspace"
  sudo -n -u tilo-browser mkdir -p /home/tilo-browser/.config/tilo/identity/device \
    /home/tilo-browser/Default
  echo not-a-real-key | sudo -n -u tilo-browser tee \
    /home/tilo-browser/.config/tilo/identity/device/keypair.bin >/dev/null
  echo not-a-real-cookie | sudo -n -u tilo-browser tee /home/tilo-browser/Default/Cookies >/dev/null
  echo source-ready') || true
printf '%s\n' "$setup" >&2
grep -qx source-ready <<<"$setup" || vm_failure 'the source VM did not take its setup'

# What a branch must hold, and what it must not: one line per finding.
branch_holds() { # workspace
  in_vm "$1" 'cat "$HOME/branch-proof"; proof-tool; cat "$HOME/.ssh/config"; cat "$HOME/.gitconfig"
    for left in "$HOME/.config/tilo" "$HOME/.local/share/tilo"; do
      [ ! -e "$left" ] || echo "left behind: $left"
    done
    sudo -n ls -A /home/tilo-browser | sed "s/^/left behind in the browser home: /"
    echo looked'
}
check_branch() { # workspace findings
  printf '%s\n' "$2" >&2
  grep -qx looked <<<"$2" || vm_failure "the branch $1 did not run"
  for line in written-in-the-source proof-tool-ran 'Host branch-proof'; do
    grep -qx "$line" <<<"$2" || die "the branch $1 lacks what the source had: $line"
  done
  grep -q 'name = Branch Proof' <<<"$2" || die "the branch $1 lacks the tenant's .gitconfig"
  if grep -q '^left behind' <<<"$2"; then die "the branch $1 holds the source's sessions"; fi
}

[ "$(phase_of ws-local)" = Running ] || die "the source is $(phase_of ws-local) at the branch, not Running"
say 'branching the running source'
as_service branch --run /run/tilo-hosted --workspace ws-branch --account acct-tilo-sh \
  --size small --from ws-local || vm_failure 'the running VM was not branched'
[ "$(phase_of ws-local)" = Running ] || die 'the source does not run on after its branch'
[ "$(phase_of ws-branch)" = Off ] || die "the branch is $(phase_of ws-branch), not a disk that boots cold"
podman exec tilo-sh-vm-host-0 sh -c "cd $(vm_dir ws-branch)/default/vmm && ls -A" > "$state/branch-files"
[ "$(cat "$state/branch-files")" = disk.raw ] \
  || die "the branch holds more than a disk: $(tr '\n' ' ' < "$state/branch-files")"
check_branch ws-branch "$(branch_holds ws-branch)"
hosted status | grep -q '^ws-branch/default .* (booted)' \
  || die 'the branch was not booted cold'

say "each machine's writes are its own"
in_vm ws-branch 'echo x > "$HOME/only-in-the-branch"' >/dev/null
seen=$(in_vm ws-local 'echo y > "$HOME/only-in-the-source"; [ -e "$HOME/only-in-the-branch" ] || echo source-alone; proof-tool; true')
grep -qx source-alone <<<"$seen" || die "a file written in the branch is in the source: $seen"
grep -qx proof-tool-ran <<<"$seen" || die "the source lost its tool: $seen"
seen=$(in_vm ws-branch '[ -e "$HOME/only-in-the-source" ] || echo branch-alone; true')
grep -qx branch-alone <<<"$seen" || die "a file written in the source is in the branch: $seen"

say 'deleting the branch'
hosted delete ws-branch default || vm_failure 'the branch was not deleted'
podman exec tilo-sh-vm-host-0 test ! -e "$(vm_dir ws-branch)" || die 'the deleted branch left its disk'
! hosted status | grep -q '^ws-branch/' || die 'the host still holds the deleted branch'
hosted delete ws-branch default || die 'deleting a deleted VM again is not done'

say 'branching a suspended source'
in_vm ws-local 'set -e
  echo written-before-the-suspend > "$HOME/suspend-proof"
  printf "#!/bin/sh\necho second-tool-ran\n" > /tmp/second-tool
  sudo -n install -m 0755 /tmp/second-tool /usr/local/bin/second-tool
  echo written' | grep -qx written || vm_failure 'the source did not write before its suspend'
hosted suspend ws-local default || vm_failure 'the source did not suspend'
[ "$(phase_of ws-local)" = Suspended ] || die "the source is $(phase_of ws-local), not Suspended"
as_service branch --run /run/tilo-hosted --workspace ws-branch2 --account acct-tilo-sh \
  --size small --from ws-local || vm_failure 'the suspended VM was not branched'
[ "$(phase_of ws-local)" = Suspended ] || die 'the suspended source is not suspended again after its branch'
seen=$(branch_holds ws-branch2)
check_branch ws-branch2 "$seen"
seen=$(in_vm ws-branch2 'cat "$HOME/suspend-proof"; second-tool; [ ! -e "$HOME/only-in-the-source" ] || echo later-source-write; true')
printf '%s\n' "$seen" >&2
grep -qx written-before-the-suspend <<<"$seen" || die 'the branch of a suspended VM lacks what it wrote last'
grep -qx second-tool-ran <<<"$seen" || die 'the branch of a suspended VM lacks the tool it installed last'
grep -qx later-source-write <<<"$seen" || die 'the branch of a suspended VM lacks what it wrote earlier'

if out=$(as_service branch --run /run/tilo-hosted --workspace ws-none-branch --account acct-tilo-sh \
  --size small --from ws-none 2>&1); then
  die 'a workspace without a VM was branched'
fi
grep -q 'workspace ws-none has no Linux VM yet' <<<"$out" || die "a branch of no VM said: $out"
podman exec tilo-sh-vm-host-0 test ! -e "$(vm_dir ws-none-branch)" || die 'a branch of no VM made one'

say 'deleting the second branch; the source runs on'
hosted delete ws-branch2 default || vm_failure 'the second branch was not deleted'
podman exec tilo-sh-vm-host-0 test ! -e "$(vm_dir ws-branch2)" || die 'the deleted branch left its disk'
seen=$(in_vm ws-local 'proof-tool; second-tool; cat "$HOME/only-in-the-source"
  cat "$HOME/.config/tilo/identity/device/keypair.bin"; true')
printf '%s\n' "$seen" >&2
for line in proof-tool-ran second-tool-ran y not-a-real-key; do
  grep -qx "$line" <<<"$seen" || die "the source after its branches lacks: $line"
done
say 'a machine was branched awake and asleep, the branches kept its tools and none of its sessions, and were deleted'

say 'draining, as a rolling update does'
hosted drain || vm_failure 'the machine did not drain'
[ "$(phase_of ws-local)" = Suspended ] || die "the drained machine holds the VM $(phase_of ws-local), not Suspended"
if out=$(in_vm ws-local 'true' 2>&1); then die 'a drained machine woke a VM'; fi
grep -q 'is moving to another machine' <<<"$out" || die "a drained machine's wake said: $out"

vm_host down --wipe
[ ! -d "$state/vm" ] || die 'down --wipe left the state disk behind'
say 'a workspace VM ran on the local stack'"'"'s VM host, and it went down clean'
