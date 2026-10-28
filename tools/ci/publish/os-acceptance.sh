#!/usr/bin/env bash
# The OS image and its dev image as devices get them, as root in a micro-VM step with a
# /dev/kvm of its own. Nothing here holds a secret or publishes. The stable channel's gate, and
# the dev disk's; main's dev images need only os-dev-gate-ARCH.
#
#   os-acceptance.sh ARCH IMAGE DISK OUT   IMAGE: os-image-ARCH's layout, DISK: os-dev-disk-ARCH's
#                                          output (the dev image installed); reports and logs into
#                                          OUT
#
# Its gates are t shards (t.toml `shards`), each a check of its own, os-acceptance-ARCH/<shard>,
# cached on its own and run again alone when it fails; each boots throwaway overlays of the
# installed disk or the OS image's own UKI:
#
#   desktop  the installed desktop from the sign-in screen on (os/dev/test-installed-desktop.sh)
#   nearby   nearby pairing, a third device through one without the identity key, sibling
#            unlock and, on x86_64, wake (os/dev/test-nearby-pairing.sh)
#   reach    device-admin reach through a relay (os/dev/test-device-admin-reach.sh)
#   joined   a joined device signs in (os/dev/test-joined-device-startup.sh)
#   splash   the boot splash on a panel larger than the firmware's mode (os/dev/check-boot-splash.py)
#
# t gives a job its shards in T_SHARDS (all of them when it gives none): one each where there is
# room, several in one VM where there isn't. Those run side by side as far as the VM's memory
# holds their guests, the longest first, and each says how it ended with a `t-shard:` line.
# Every gate's report and its guests' logs go to OUT/<shard>.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-acceptance.sh ARCH IMAGE DISK OUT}
image=${2:?}
disk=${3:?}/disk/disk.qcow2
out=${4:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch image is checked natively, not on $(uname -m)"
case $arch in
  x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga qemu-device-display-virtio-gpu) ;;
  aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu qemu-device-display-virtio-gpu-pci) ;;
  *) die "unknown architecture $arch" ;;
esac
[[ -f $image/oci/index.json ]] || die "$image holds no image layout (oci/)"
[[ -s $disk ]] || die "no installed disk at $disk"
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'

missing=()
for p in dosfstools btrfs-progs systemd-container binutils socat qemu-img ipxe-roms-qemu sshpass \
  openssh-clients jq cryptsetup mtools zstd procps-ng "${qemu[@]}"; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
command -v podman >/dev/null || missing+=(podman skopeo fuse-overlayfs e2fsprogs util-linux python3)
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
use_cosign
rm -rf "$out"
mkdir -p "$out"
status=0

export TILO_VM_ARCH=$arch TMPDIR=/var/tmp TILO_USER=tester TILO_PASS=tilo

# A host without a usable render node boots the guests on the 2D virtio display; the
# compositor paints on the CPU there, and every gate must pass either way.
export TILO_VM_DISPLAY=2d
for node in /dev/dri/renderD*; do
  [[ -c $node && -r $node && -w $node ]] && { TILO_VM_DISPLAY=gl; break; }
done

wake=()
if [[ $arch == x86_64 ]]; then wake=(--wake); fi
shard_desktop() {
  bash os/dev/test-installed-desktop.sh "$disk" --out "$out/desktop/installed-desktop" --base-digest "$(cat "$image/digest")"
}
shard_nearby() {
  bash os/dev/test-nearby-pairing.sh "$disk" --out "$out/nearby/nearby-pairing" --unlock "${wake[@]}"
}
shard_reach() {
  bash os/dev/test-device-admin-reach.sh "$disk" --out "$out/reach/device-admin-reach"
}
shard_joined() {
  # Its own SSH port: nearby's third guest takes the gate's default, 2233, in the same VM.
  TILO_JOINED_SSH_PORT=2234 bash os/dev/test-joined-device-startup.sh "$disk" --out "$out/joined/joined-device"
}
shard_splash() {
  python3 os/dev/check-boot-splash.py --arch "$arch" --out "$out/splash/boot-splash" "oci:$image/oci:tilo"
}
# GiB of the VM's memory each shard's guests take at most (os/dev/_common.sh TILO_MEM per guest,
# with QEMU's own beside it): nearby runs three guests at its peak, reach two and a relay.
declare -A gib=([desktop]=5 [nearby]=13 [reach]=11 [joined]=5 [splash]=4)
read -r -a shards <<<"${T_SHARDS:-desktop nearby reach joined splash}"
for shard in "${shards[@]}"; do
  [[ -n ${gib[$shard]:-} ]] || die "unknown shard $shard"
done
budget=$(($(awk '/^MemTotal:/ { print int($2 / 1048576) }' /proc/meminfo) - 3))
echo "os-acceptance: shards ${shards[*]} in ${budget} GiB at ${SECONDS}s"

# A failed check keeps no outputs, so the guests' own account of a failed shard goes to the log
# as it ends: the end of each session's journal and compositor log its gate collected, without
# the periodic frame counters and SSH logins that would fill it.
evidence() {
  local dir=$1 file
  while IFS= read -r -d '' file; do
    printf '\n--- %s (last 80 lines of what it said)\n' "${file#"$out"/}"
    grep -avE '^(paint-stats|frames)[:=]|sshd-session\[' "$file" | tail -n 80
  done < <(find "$dir" -type f \( -name '*session-journal.txt' -o -name '*compositor-session.log' \) -print0 | sort -z)
  # A frozen compositor's record opens with its main thread, the one that froze.
  while IFS= read -r -d '' file; do
    printf '\n--- %s (first 80 lines)\n' "${file#"$out"/}"
    head -n 80 "$file"
  done < <(find "$dir" -type f -name '*compositor-freeze.log' -size +0 -print0 | sort -z)
}

declare -A pid_shard=()
used=0
start_shard() {
  local shard=$1
  mkdir -p "$out/$shard"
  # Each line carries the seconds since the shard started, so a slow phase shows in the log.
  ( set -o pipefail; "shard_$shard" 2>&1 | tee "$out/$shard/$shard.log" \
      | awk -v t="$shard" -v s="$(date +%s)" '{ printf "[%s +%ds] %s\n", t, systime() - s, $0; fflush() }' ) &
  pid_shard[$!]=$shard
  used=$((used + gib[$shard]))
}
pending=("${shards[@]}")
failed=()
# The desktop gate times its own boot and the sign-in screen's keystrokes against budgets made
# for a quiet machine, so where it shares the VM it boots first, alone, and the others start once
# it signs in (its log's "logging in" line from os/dev/vm.sh) or ends.
if ((${#shards[@]} > 1)) && [[ ${shards[0]} == desktop ]]; then
  start_shard desktop
  pending=("${shards[@]:1}")
  desktop_pid=$(for p in "${!pid_shard[@]}"; do echo "$p"; done)
  while kill -0 "$desktop_pid" 2>/dev/null \
    && ! grep -q "logging in with the installed user's passcode" "$out/desktop/desktop.log" 2>/dev/null; do
    sleep 1
  done
  echo "os-acceptance: desktop signs in at ${SECONDS}s; the other shards start"
fi
while ((${#pending[@]} > 0 || ${#pid_shard[@]} > 0)); do
  # Start what fits beside what runs; a shard alone runs whatever it takes.
  rest=()
  for shard in "${pending[@]}"; do
    if ((${#pid_shard[@]} == 0 || used + gib[$shard] <= budget)); then
      start_shard "$shard"
    else
      rest+=("$shard")
    fi
  done
  pending=("${rest[@]}")
  rc=0
  wait -n -p done_pid "${!pid_shard[@]}" || rc=$?
  shard=${pid_shard[$done_pid]}
  unset "pid_shard[$done_pid]"
  used=$((used - gib[$shard]))
  if ((rc == 0)); then
    echo "ok    $shard at ${SECONDS}s"
    echo "t-shard: $shard passed"
  else
    echo "FAIL  $shard at ${SECONDS}s (its log: $out/$shard/$shard.log)"
    failed+=("$shard")
    evidence "$out/$shard"
    echo "t-shard: $shard failed"
  fi
done
((${#failed[@]} == 0)) || status=1
exit "$status"
