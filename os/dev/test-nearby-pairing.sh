#!/usr/bin/env bash
# Two booted Tilo disks on one Ethernet segment: joining an existing identity
# from first-boot onboarding through "Use my existing Tilo identity -> Connect a
# nearby device".
#
# VM A finishes onboarding by creating an identity. VM B stays at its first
# login onboarding and asks to join it. Both boot the same installed disk (as
# throwaway overlays) and share a second NIC on one L2 segment (os/dev/vm.sh
# TILO_VM_LAN_*), so the pairing broadcast on UDP 23831 crosses between them
# exactly as it does on a home network. B's UI is driven over AT-SPI like the
# release gate drives first use; A approves as its signed-in user with
# tilo-enroll, the program Settings runs.
#
# The run proves, in order:
#   1. B's enrollment state directories are part of the image layout and
#      writable by the device-trust group, so pairing needs no administrator.
#   2. A pairing helper failure (enrollment-commits made unwritable, the
#      original field failure) reaches B's status file and its UI as an error,
#      and the nearby action is offered again.
#   3. A retry pairs: B holds a certificate from A's identity, B's fleet record
#      lists B, the status file and UI report completion, onboarding leaves the
#      Connect screen, and no polkit prompt or privileged helper was involved.
#      The join keeps the device-trust ownership layout and records the
#      account's active identity.
#   4. Signing out and in again resumes after the identity: no identity choice,
#      no recovery screen, and the identity verifies at login unchanged.
#
# With --wake (x86_64, a dev disk carrying tilo-control), the paired devices
# then reach and wake each other:
#   5. A's remote action reaches B, where it holds idle-suspend off with a
#      "Remote use from" lease.
#   6. B suspends to RAM. A invokes `device.wake` for B; a magic packet for
#      B's MAC appears on the segment (captured on A's side of it). The harness
#      resumes B with `system_wakeup`, standing in for the NIC, and A reports
#      that B rejoined. A's next remote action reaches B unchanged.
#   7. B's links are cut and restored, and A's remote action reaches B again.
#
# With --unlock (a dev disk carrying tilo-control), B's root then moves onto
# LUKS2 with a sibling-unlock slot, and B reboots several times
# (os/dev/nearby-unlock-phase.sh):
#   8. A approves B's unlock through `device.unlock`, and B boots without a
#      passphrase.
#   9. A denial, a replay of the approved exchange, a boot key that was never
#      certified, and B's removal from the fleet each leave B at the
#      passphrase prompt, with the attempt in A's history.
#
# Usage: os/dev/test-nearby-pairing.sh DISK_QCOW2 [--out DIR] [--display gl|2d] [--wake] [--unlock]
#
# DISK_QCOW2 is never booted or modified; each VM boots its own overlay.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
vm="$dev_dir/vm.sh"
base="${1:?usage: test-nearby-pairing.sh DISK_QCOW2 [--out DIR] [--display gl|2d]}"
shift
out="$PWD/nearby-pairing-artifacts"
display="${TILO_VM_DISPLAY:-gl}"
wake=0
unlock=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --display) display="${2:?--display needs gl or 2d}"; shift 2 ;;
    --wake) wake=1; shift ;;
    --unlock) unlock=1; shift ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
case "$display" in gl|2d) ;; *) echo '--display must be gl or 2d' >&2; exit 2 ;; esac
[ -f "$base" ] || { printf 'disk not found: %s\n' "$base" >&2; exit 1; }
base="$(cd "$(dirname "$base")" && pwd)/$(basename "$base")"
command -v jq >/dev/null || { echo 'jq is required' >&2; exit 1; }

export TILO_USER="${TILO_USER:-quickuser}"
export TILO_PASS="${TILO_PASS:-tilo}"
arch="${TILO_VM_ARCH:-$(uname -m)}"
# Short: QEMU's monitor and console sockets live here (108-byte AF_UNIX limit).
run_a="${TILO_NEARBY_RUN_A:-/var/tmp/tilo-nearby-a-$arch}"
run_b="${TILO_NEARBY_RUN_B:-/var/tmp/tilo-nearby-b-$arch}"
lan_a=192.168.231.10
lan_b=192.168.231.11
mac_a=52:54:00:23:83:0a
mac_b=52:54:00:23:83:0b
identity_dir='$HOME/.config/tilo/identity'
status_file="$identity_dir/.nearby-enrollment-status.json"

mkdir -p "$out"
rm -rf -- "$run_a" "$run_b"
install -d -m 0700 "$run_a" "$run_b"

die() { printf 'nearby pairing: %s\n' "$*" >&2; exit 1; }
note() { printf 'nearby pairing: %s\n' "$*" >&2; }

vm_a() {
  TILO_RUN="$run_a" TILO_SSH_PORT="${TILO_NEARBY_SSH_A:-2231}" \
    TILO_VM_LAN_PORT=23901 TILO_VM_LAN_PEER=23902 TILO_VM_LAN_MAC="$mac_a" \
    "$vm" "$@"
}
# For --wake, B suspends to RAM and its segment NIC is one that takes a
# Wake-on-LAN setting.
b_lan_model=virtio-net-pci
[ "$wake" = 0 ] || b_lan_model=e1000
# Wait until B's onboarding shows a node whose name contains each fragment.
# `vm.sh ssh` hands its words to the guest's shell, so each fragment is quoted
# for it.
b_shows() {
  vm_b ssh "/usr/libexec/tilo/native-atspi-smoke.sh oobe-text $(printf '%q ' "$@")"
}
vm_b() {
  TILO_RUN="$run_b" TILO_SSH_PORT="${TILO_NEARBY_SSH_B:-2232}" \
    TILO_VM_LAN_PORT=23902 TILO_VM_LAN_PEER=23901 TILO_VM_LAN_MAC="$mac_b" \
    TILO_VM_LAN_MODEL="$b_lan_model" TILO_VM_S3="$wake" \
    "$vm" "$@"
}
# Root on the guest for test setup only (the LAN address, the injected
# failure). The account is in wheel; sudo takes the password's line from
# stdin and the shell it starts reads the script that follows.
sudo_a() { printf '%s\n%s\n' "$TILO_PASS" "$1" | vm_a ssh "sudo -S -p '' sh -s"; }
sudo_b() { printf '%s\n%s\n' "$TILO_PASS" "$1" | vm_b ssh "sudo -S -p '' sh -s"; }

collect() {
  local label=$1 run=$2 each=$3
  [ ! -f "$run/serial.log" ] || cp -f "$run/serial.log" "$out/$label-serial.log"
  [ ! -f "$run/qemu.log" ] || cp -f "$run/qemu.log" "$out/$label-qemu.log"
  "$each" ssh 'journalctl -b --no-pager -o short-monotonic -n 400 _UID=$(id -u)' \
    > "$out/$label-session-journal.txt" 2>&1 || true
  "$each" ssh 'journalctl -b --no-pager -o short-monotonic -u polkit.service' \
    > "$out/$label-polkit-journal.txt" 2>&1 || true
  "$each" ssh "cat $status_file" > "$out/$label-status.json" 2>/dev/null || true
  "$each" ssh 'ls -la /var/lib/tilo /var/lib/tilo/device; ip -br addr; ip route; ps -eo pid,user,args' \
    > "$out/$label-state.txt" 2>&1 || true
  "$each" shot "$label-final" >/dev/null 2>&1 \
    && cp -f "$run/$label-final.png" "$out/" 2>/dev/null || true
}
cleanup() {
  local rc=$?
  trap - EXIT
  collect a "$run_a" vm_a
  collect b "$run_b" vm_b
  vm_a stop >/dev/null 2>&1 || true
  vm_b stop >/dev/null 2>&1 || true
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  exit "$rc"
}
trap cleanup EXIT

# --- boot both machines -------------------------------------------------------
qemu-img create -f qcow2 -F qcow2 -b "$base" "$run_a/disk.qcow2" >/dev/null
qemu-img create -f qcow2 -F qcow2 -b "$base" "$run_b/disk.qcow2" >/dev/null
# --unlock moves B's root onto a new LUKS2 partition at least its own size;
# the overlay stays sparse, so only the data actually copied takes host space.
[ "$unlock" = 0 ] || qemu-img resize -q "$run_b/disk.qcow2" "+$(qemu-img info --output=json "$base" | jq -r '."virtual-size"')"
vm_a boot "--$display" --disk "$run_a/disk.qcow2"
vm_b boot "--$display" --disk "$run_b/disk.qcow2"
vm_a wait
vm_b wait
for each in vm_a vm_b; do
  for attempt in $(seq 1 60); do
    "$each" ssh true >/dev/null 2>&1 && break
    [ "$attempt" -lt 60 ] || die "$each: no SSH after boot"
    sleep 2
  done
done

# The second NIC gets a fixed address and the lower-metric default route, so
# the limited broadcast a pairing device sends leaves through the shared
# segment rather than the VM's private user-mode NAT.
configure_lan() {
  local each_sudo=$1 mac=$2 address=$3 wol=${4:-default}
  "$each_sudo" "
    set -eu
    iface=\$(ip -o link | awk -F': ' -v mac='$mac' 'index(tolower(\$0), mac) { print \$2; exit }')
    [ -n \"\$iface\" ]
    nmcli connection add type ethernet ifname \"\$iface\" con-name tilo-nearby-lan \
      ipv4.method manual ipv4.addresses $address/24 ipv4.gateway 192.168.231.1 \
      ipv4.route-metric 10 ipv6.method disabled 802-3-ethernet.wake-on-lan $wol >/dev/null
    nmcli connection up tilo-nearby-lan >/dev/null
    ip route get 255.255.255.255 | grep -q \"dev \$iface \"
  "
}
configure_lan sudo_a "$mac_a" "$lan_a" || die "VM A: shared segment could not be configured"
b_wol=default
[ "$wake" = 0 ] || b_wol=magic
configure_lan sudo_b "$mac_b" "$lan_b" "$b_wol" || die "VM B: shared segment could not be configured"
if vm_a ssh 'command -v ping >/dev/null'; then
  for attempt in $(seq 1 30); do
    vm_a ssh "ping -c 1 -W 2 $lan_b" >/dev/null 2>&1 && break
    [ "$attempt" -lt 30 ] || die "VM A cannot reach VM B over the shared segment"
    sleep 1
  done
fi

# --- both reach first-use onboarding ------------------------------------------
wait_onboarding() {
  local each=$1
  "$each" login --passcode-only "$TILO_PASS"
  for attempt in $(seq 1 120); do
    "$each" ssh 'busctl --user status org.freedesktop.impl.portal.desktop.tilo >/dev/null 2>&1' && return 0
    sleep 1
  done
  die "$each: the compositor did not reach first-login onboarding"
}
wait_onboarding vm_a
wait_onboarding vm_b

# A: the ordinary Create path, exactly as the release gate makes it.
vm_a ssh /usr/libexec/tilo/native-atspi-smoke.sh oobe-actions
for attempt in $(seq 1 60); do
  vm_a ssh "test -s $identity_dir/primary.json && test -s /var/lib/tilo/device/cert.bin" && break
  [ "$attempt" -lt 60 ] || die "VM A did not create its identity"
  sleep 1
done
vm_a keys tab ret
identity="$(vm_a ssh 'od -An -v -tx1 /var/lib/tilo/user-identity/pubkey.bin' | tr -d ' \n')"
[[ $identity =~ ^[0-9a-f]{64}$ ]] || die "VM A has no readable identity public key"
note "VM A holds identity $identity"

# --- the image owns the enrollment layout --------------------------------------
layout="$(vm_b ssh 'stat -c "%n %a %U %G" /var/lib/tilo/enrollment-staging /var/lib/tilo/enrollment-commits /var/lib/tilo/peers')"
printf '%s\n' "$layout" > "$out/b-enrollment-layout.txt"
for dir in enrollment-staging enrollment-commits peers; do
  grep -qx "/var/lib/tilo/$dir 2770 root tilo-identity" <<<"$layout" \
    || die "/var/lib/tilo/$dir is not 2770 root:tilo-identity on a fresh install: $layout"
done
vm_b ssh "id -nG | tr ' ' '\n' | grep -qx tilo-identity" \
  || die "the installed account is not in the device-trust group"
# The device key belongs to the image's first boot; joining must not touch it.
keypair_before="$(vm_b ssh 'stat -c "%a %U %G" /var/lib/tilo/device-private/keypair.bin')"
[[ $keypair_before == "640 "*" tilo-identity" ]] \
  || die "B's device key is not 0640 in the device-trust group on a fresh install: $keypair_before"

# --- one attempt: B asks, A approves -------------------------------------------
# Prints the pairing code B published once it is waiting for approval.
await_code() {
  local status code
  for attempt in $(seq 1 120); do
    status="$(vm_b ssh "cat $status_file" 2>/dev/null || true)"
    if [ "$(jq -r '.state // empty' <<<"$status" 2>/dev/null)" = waiting_for_approval ]; then
      code="$(jq -r '.verification_code // empty' <<<"$status")"
      [[ $code =~ ^[0-9]{6}$ ]] || die "B's status file carries no six-digit code: $status"
      printf '%s\n' "$code"
      return 0
    fi
    sleep 1
  done
  die "B never reported waiting_for_approval: ${status:-no status file}"
}
# A discovers the asking device, checks the code both sides show, approves.
approve_from_a() {
  local code=$1 log=$2 candidate device service
  candidate="$(vm_a ssh 'tilo-enroll discover-device --timeout 120')" \
    || die "VM A did not hear B's pairing broadcast"
  printf '%s\n' "$candidate" > "$out/$log-candidate.json"
  [ "$(jq -r .verification_code <<<"$candidate")" = "$code" ] \
    || die "A sees code $(jq -r .verification_code <<<"$candidate"), B shows $code"
  device="$(jq -r .device_node_id <<<"$candidate")"
  service="$(jq -r .relay_service_node_id <<<"$candidate")"
  printf '%s\n' "$device" > "$out/$log-device.txt"
  # The signed-in person's own session bus answers for their identity record.
  vm_a ssh "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/\$(id -u)/bus \
    tilo-enroll approve-device --identity-root /var/lib/tilo \
      --identity-dir $identity_dir --timeout 90 \
      --expected-device $device --expected-service $service" \
    > "$out/$log-approve.txt" 2>&1
}
await_state() {
  local wanted=$1 seconds=$2 status
  for _ in $(seq 1 "$seconds"); do
    status="$(vm_b ssh "cat $status_file" 2>/dev/null || true)"
    if [ "$(jq -r '.state // empty' <<<"$status" 2>/dev/null)" = "$wanted" ]; then
      printf '%s\n' "$status"
      return 0
    fi
    sleep 1
  done
  die "B's status never reached $wanted: ${status:-no status file}"
}
no_privileged_pairing() {
  # Pairing never involves an administrator: nothing waits on a polkit agent
  # and polkit was never asked. The probe names processes by what they run,
  # not by a pattern in a command line, which matched the remote shell
  # carrying it.
  vm_b ssh 'sh -s' < "$dev_dir/nearby-privileged-processes.sh" \
    || die "a privileged helper or polkit prompt is pending on B"
  if vm_b ssh 'journalctl -b --no-pager -o cat -u polkit.service' \
      | grep -E 'org\.tilo\.|FAILED to authenticate|tilo-home-peer-control'; then
    die "B's pairing asked polkit"
  fi
}

# --- negative: the helper cannot commit ----------------------------------------
# Exactly the field failure: the approval arrives, then publishing it is
# refused with EACCES. The UI must say so and offer the action again.
sudo_b 'chmod 2750 /var/lib/tilo/enrollment-commits'
vm_b ssh /usr/libexec/tilo/native-atspi-smoke.sh oobe-nearby
code="$(await_code)"
b_shows "${code:0:3} ${code:3:3}"
approve_from_a "$code" refused || note "A's approval did not complete, as expected for a refused commit"
failed="$(await_state failed 150)"
printf '%s\n' "$failed" > "$out/b-status-failed.json"
jq -e '.error | test("Permission denied")' <<<"$failed" >/dev/null \
  || die "the failed status does not carry the helper's error: $failed"
b_shows "Permission denied" "Try again"
for attempt in $(seq 1 30); do
  vm_b ssh '! pgrep -u "$(id -u)" -x tilo-enroll >/dev/null' && break
  [ "$attempt" -lt 30 ] || die "the failed pairing helper is still running on B"
  sleep 1
done
vm_b ssh '! test -e /var/lib/tilo/device/cert.bin' \
  || die "a failed pairing left a device certificate on B"
no_privileged_pairing
vm_b shot negative-error >/dev/null && cp -f "$run_b/negative-error.png" "$out/"

# --- retry: the same UI pairs -----------------------------------------------------
sudo_b 'chmod 2770 /var/lib/tilo/enrollment-commits'
vm_b ssh /usr/libexec/tilo/native-atspi-smoke.sh oobe-nearby
code="$(await_code)"
b_shows "${code:0:3} ${code:3:3}"
approve_from_a "$code" paired || {
  cat "$out/paired-approve.txt" >&2
  die "A's approval failed"
}
complete="$(await_state complete 150)"
printf '%s\n' "$complete" > "$out/b-status-complete.json"
[ "$(jq -r .identity <<<"$complete")" = "$identity" ] \
  || die "B completed into another identity: $complete"
record="$(jq -r .record_path <<<"$complete")"
vm_b ssh "test -s $(printf '%q' "$record")" || die "B's completed status names no identity record: $record"
device="$(cat "$out/paired-device.txt")"

# B is enrolled: its certificate and key binding name A's identity, and the
# fleet record both sides keep lists B.
vm_b ssh 'test -s /var/lib/tilo/device/cert.bin' || die "B has no device certificate"
[ "$(vm_b ssh 'od -An -v -tx1 /var/lib/tilo/user-identity/pubkey.bin' | tr -d ' \n')" = "$identity" ] \
  || die "B's device is bound to another identity"
fleet_lists_b() {
  # Keys serialize as byte arrays; compare them as the hex the CLI prints.
  jq -e --arg device "$device" --arg identity "$identity" '
    def hex: map([(. / 16 | floor), (. % 16)] | map("0123456789abcdef"[.:.+1]) | add) | add;
    .identity_id == $identity and
    any(.certificates[];
        (.device_public_key | hex) == $device and (.user_public_key | hex) == $identity)
  ' >/dev/null
}
vm_b ssh "cat $identity_dir/fleets/$identity.json" > "$out/b-fleet.json" \
  || die "B has no fleet record for $identity"
fleet_lists_b < "$out/b-fleet.json" || die "B's fleet record does not list B"
vm_a ssh "cat $identity_dir/fleets/$identity.json" > "$out/a-fleet.json" \
  || die "A has no fleet record for $identity"
fleet_lists_b < "$out/a-fleet.json" || die "A's fleet record does not list B"

vm_b ssh /usr/libexec/tilo/native-atspi-smoke.sh oobe-nearby-done
no_privileged_pairing
vm_b shot paired >/dev/null && cp -f "$run_b/paired.png" "$out/"

# The joined record is the account's active identity: a private file owned by
# the person, naming the record's stem.
stem="$(basename "$record" .json)"
active="$(vm_b ssh "stat -c '%a %U' $identity_dir/active && cat $identity_dir/active")"
printf '%s\n' "$active" > "$out/b-active.txt"
[ "$(sed -n 1p <<<"$active")" = "600 $TILO_USER" ] \
  || die "B's active identity pointer is not 0600 $TILO_USER: $active"
sed -n '2,$p' <<<"$active" | grep -Fq "$stem" \
  || die "B's active identity pointer does not name $stem: $active"

# Joining keeps the device-trust layout: the directories stay root-owned and
# group-shared, and what the join wrote stays readable by the group the
# session and relay read the device identity through.
layout_after="$(vm_b ssh 'stat -c "%n %a %U %G" /var/lib/tilo/user-identity /var/lib/tilo/device /var/lib/tilo/peers /var/lib/tilo/device-private /var/lib/tilo/device-private/keypair.bin')"
printf '%s\n' "$layout_after" > "$out/b-layout-after-join.txt"
for expected in \
  '/var/lib/tilo/user-identity 2770 root tilo-identity' \
  '/var/lib/tilo/device 2770 root tilo-identity' \
  '/var/lib/tilo/peers 2770 root tilo-identity' \
  '/var/lib/tilo/device-private 2750 root tilo-identity' \
  "/var/lib/tilo/device-private/keypair.bin $keypair_before"; do
  grep -qxF "$expected" <<<"$layout_after" \
    || die "joining changed the device-trust layout; expected '$expected' in: $layout_after"
done
joined="$(vm_b ssh 'find /var/lib/tilo/device/cert.bin /var/lib/tilo/device/certs /var/lib/tilo/user-identity/pubkey.bin -printf "%p %y %G\n"')"
printf '%s\n' "$joined" > "$out/b-joined-files.txt"
grep -q '^/var/lib/tilo/device/certs d ' <<<"$joined" || die "the join wrote no per-identity certificate directory: $joined"
grep -q '^/var/lib/tilo/device/certs/.* f ' <<<"$joined" || die "the join wrote no per-identity certificate: $joined"
if awk '$3 != "tilo-identity"' <<<"$joined" | grep -q .; then
  die "the join wrote files outside the device-trust group: $joined"
fi
# Read as the session's own account, through the group, not as their owner.
sudo_b "set -eu
  find /var/lib/tilo/device/cert.bin /var/lib/tilo/device/certs /var/lib/tilo/user-identity/pubkey.bin \
    /var/lib/tilo/device-private/keypair.bin -type f \
    -exec sudo -u '$TILO_USER' cat {} + >/dev/null
  find /var/lib/tilo/device/certs -type d -exec sudo -u '$TILO_USER' ls {} + >/dev/null" \
  || die "the session account cannot read what the join wrote"
cert_before="$(vm_b ssh 'sha256sum /var/lib/tilo/device/cert.bin')"
status_before="$(vm_b ssh "cat $status_file")"

# --- a new session resumes after the identity, never before it -----------------
# End the graphical session the way logind does for a logout and sign in again
# through the greeter. First use must not start over at the identity choice.
sudo_b "loginctl list-sessions --no-legend | awk '\$4 == \"seat0\" { print \$1 }' \
  | while read -r session; do loginctl terminate-session \"\$session\"; done"
vm_b wait-greeter-return
vm_b login --passcode-only "$TILO_PASS"
for attempt in $(seq 1 120); do
  vm_b ssh 'busctl --user status org.freedesktop.impl.portal.desktop.tilo >/dev/null 2>&1' && break
  [ "$attempt" -lt 120 ] || die "B's compositor did not return after signing in again"
  sleep 1
done
vm_b ssh /usr/libexec/tilo/native-atspi-smoke.sh post-identity
vm_b shot relogin >/dev/null && cp -f "$run_b/relogin.png" "$out/"
# The read-only gate the graphical session wrapper runs at login, in the
# person's own session so their secret store answers.
vm_b ssh 'DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus /usr/bin/shell --verify-collaboration-identity' \
  > "$out/b-verify-identity.txt" 2>&1 || {
    cat "$out/b-verify-identity.txt" >&2
    die "B's joined identity does not verify at login"
  }
[ "$(vm_b ssh 'sha256sum /var/lib/tilo/device/cert.bin')" = "$cert_before" ] \
  || die "signing in again replaced B's device certificate"
[ "$(vm_b ssh "cat $status_file" 2>/dev/null || true)" = "$status_before" ] \
  || die "signing in again changed B's pairing status"
[ "$(vm_b ssh 'od -An -v -tx1 /var/lib/tilo/user-identity/pubkey.bin' | tr -d ' \n')" = "$identity" ] \
  || die "signing in again changed B's identity"
no_privileged_pairing
note "B joined A's identity from first-boot onboarding"

if [ "$wake" = 1 ]; then
  # shellcheck source=os/dev/nearby-wake-phase.sh
  source "$dev_dir/nearby-wake-phase.sh"
fi
if [ "$unlock" = 1 ]; then
  # shellcheck source=os/dev/nearby-unlock-phase.sh
  source "$dev_dir/nearby-unlock-phase.sh"
fi
printf 'PASS\n' > "$out/verdict.txt"
