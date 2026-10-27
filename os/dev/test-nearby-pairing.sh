#!/usr/bin/env bash
# Two booted Tilo disks on one Ethernet segment: joining an existing identity
# from first-boot onboarding through "Use my existing Tilo identity -> Connect a
# nearby device".
#
# VM A finishes onboarding by creating an identity. VM B stays at its first
# login onboarding and asks to join it. Both boot the same installed disk (as
# throwaway overlays) and share a second NIC on one L2 segment (os/dev/vm.sh
# TILO_VM_LAN_*, joined by os/dev/nearby-lan-hub.py), so the pairing broadcast
# on UDP 23831 crosses between them exactly as it does on a home network. B's
# UI is driven over AT-SPI like the release gate drives first use; A approves
# as its signed-in user with tilo-enroll, the program Settings runs. A third
# overlay, C, later joins the segment.
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
#   5. B, which holds none of the identity's key, pairs C (`tilo-identity pair`
#      there, `tilo-identity approve` on B). Nothing joins until A, which holds
#      the key, lists C among the devices asking (`tilo-identity asking`, the
#      headless face of "A device asks to be you") and approves it; then C
#      holds a certificate the identity's key signed.
#
# With --wake (x86_64, a dev disk carrying tilo-control), the paired devices
# then reach and wake each other:
#   6. A's remote action reaches B, where it holds idle-suspend off with a
#      "Remote use from" lease.
#   7. B suspends to RAM. A invokes `device.wake` for B; a magic packet for
#      B's MAC appears on the segment (captured on A's side of it). The harness
#      resumes B with `system_wakeup`, standing in for the NIC, and A reports
#      that B rejoined. A's next remote action reaches B unchanged.
#   8. B's links are cut and restored, and A's remote action reaches B again.
#
# With --unlock (a dev disk carrying tilo-control), B's root then moves onto
# LUKS2 with a sibling-unlock slot, and B reboots several times
# (os/dev/nearby-unlock-phase.sh):
#   9. A approves B's unlock through `device.unlock`, and B boots without a
#      passphrase.
#  10. A denial, a replay of the approved exchange, a boot key that was never
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
run_c="${TILO_NEARBY_RUN_C:-/var/tmp/tilo-nearby-c-$arch}"
lan_a=192.168.231.10
lan_b=192.168.231.11
lan_c=192.168.231.12
mac_a=52:54:00:23:83:0a
mac_b=52:54:00:23:83:0b
mac_c=52:54:00:23:83:0c
# Loopback UDP ports: the segment's hub, then each VM's NIC.
lan_hub=23900
identity_dir='$HOME/.config/tilo/identity'
status_file="$identity_dir/.nearby-enrollment-status.json"

mkdir -p "$out"
rm -rf -- "$run_a" "$run_b" "$run_c"
install -d -m 0700 "$run_a" "$run_b" "$run_c"

die() { printf 'nearby pairing: %s\n' "$*" >&2; exit 1; }
note() { printf 'nearby pairing: %s\n' "$*" >&2; }

vm_a() {
  TILO_RUN="$run_a" TILO_SSH_PORT="${TILO_NEARBY_SSH_A:-2231}" \
    TILO_VM_LAN_PORT=23901 TILO_VM_LAN_PEER="$lan_hub" TILO_VM_LAN_MAC="$mac_a" \
    "$vm" "$@"
}
# For --wake, B suspends to RAM and its segment NIC is one that takes a
# Wake-on-LAN setting.
b_lan_model=virtio-net-pci
[ "$wake" = 0 ] || b_lan_model=e1000
# Wait until B's onboarding shows a node whose name contains each fragment.
# `vm.sh test` hands its words to the guest's shell, so each fragment is quoted
# for it.
b_shows() {
  vm_b test native-atspi-smoke.sh "oobe-text $(printf '%q ' "$@")"
}
vm_b() {
  TILO_RUN="$run_b" TILO_SSH_PORT="${TILO_NEARBY_SSH_B:-2232}" \
    TILO_VM_LAN_PORT=23902 TILO_VM_LAN_PEER="$lan_hub" TILO_VM_LAN_MAC="$mac_b" \
    TILO_VM_LAN_MODEL="$b_lan_model" TILO_VM_S3="$wake" \
    "$vm" "$@"
}
# Root on the guest for test setup only (the LAN address, the injected
# failure). The account is in wheel; sudo takes the password's line from
# stdin and the shell it starts reads the script that follows.
sudo_a() { printf '%s\n%s\n' "$TILO_PASS" "$1" | vm_a ssh "sudo -S -p '' sh -s"; }
vm_c() {
  TILO_RUN="$run_c" TILO_SSH_PORT="${TILO_NEARBY_SSH_C:-2233}" \
    TILO_VM_LAN_PORT=23903 TILO_VM_LAN_PEER="$lan_hub" TILO_VM_LAN_MAC="$mac_c" \
    "$vm" "$@"
}
sudo_b() { printf '%s\n%s\n' "$TILO_PASS" "$1" | vm_b ssh "sudo -S -p '' sh -s"; }
sudo_c() { printf '%s\n%s\n' "$TILO_PASS" "$1" | vm_c ssh "sudo -S -p '' sh -s"; }
# Everything this boot's session on $1 recorded, each line once: the journal's
# copy and the compositor's own flight record, whichever carries it. Each
# record line starts with its own timestamp, so no two events collapse.
session_log() {
  "$1" ssh 'journalctl -b --no-pager -o cat _UID=$(id -u) 2>/dev/null
    cat "$HOME"/.local/state/tilo/session-logs/latest.log 2>/dev/null
    true' 2>/dev/null | sort -u
}
# The personal devices $1 records: the list remote actions and wake address.
personal_devices() {
  "$1" ssh 'cat /var/lib/tilo/peers/personal-device-peers.json 2>/dev/null || printf "[]"'
}

collect() {
  local label=$1 run=$2 each=$3
  [ ! -f "$run/serial.log" ] || cp -f "$run/serial.log" "$out/$label-serial.log"
  [ ! -f "$run/qemu.log" ] || cp -f "$run/qemu.log" "$out/$label-qemu.log"
  "$each" ssh 'journalctl -b --no-pager -o short-monotonic -n 400 _UID=$(id -u)' \
    > "$out/$label-session-journal.txt" 2>&1 || true
  "$each" ssh 'journalctl -b --no-pager -o short-monotonic -u polkit.service' \
    > "$out/$label-polkit-journal.txt" 2>&1 || true
  # The compositor logs to the session's state directory, not the journal.
  "$each" ssh 'cat "$HOME"/.local/state/tilo/session-logs/*.log' \
    > "$out/$label-compositor-session.log" 2>&1 || true
  "$each" ssh 'cat "$HOME"/.cache/tilo/crashes/*-freeze.log 2>/dev/null' \
    > "$out/$label-compositor-freeze.log" 2>&1 || true
  "$each" ssh "cat $status_file" > "$out/$label-status.json" 2>/dev/null || true
  "$each" ssh 'ls -la /var/lib/tilo /var/lib/tilo/device; ip -br addr; ip route; ps -eo pid,user,args' \
    > "$out/$label-state.txt" 2>&1 || true
  "$each" shot "$label-final" >/dev/null 2>&1 \
    && cp -f "$run/$label-final.png" "$out/" 2>/dev/null || true
}
boot_owned_vm() {
  local label=$1 run=$2 each=$3 boot_rc=0 identity_rc=0
  "$each" boot "--$display" --disk "$run/disk.qcow2" || boot_rc=$?
  printf '%s\n' "$boot_rc" > "$out/$label-boot-exit-status.txt"
  "$each" tool python3 "$dev_dir/nearby-vm-identity.py" snapshot "$run/qemu.pid" \
    > "$out/$label-qemu-identity.json" 2> "$out/$label-qemu-identity-error.txt" || identity_rc=$?
  [ "$boot_rc" = 0 ] || return "$boot_rc"
  return "$identity_rc"
}
stop_owned_vm() {
  local label=$1 each=$2 identity="$out/$1-qemu-identity.json"
  if ! "$each" tool python3 "$dev_dir/nearby-vm-identity.py" before-stop "$identity" \
      > "$out/$label-cleanup-before.json" 2> "$out/$label-cleanup-before-error.txt"; then
    return 1
  fi
  local stop_rc=0 verify_rc=0
  "$each" stop > "$out/$label-cleanup-stop.txt" 2>&1 || stop_rc=$?
  "$each" tool python3 "$dev_dir/nearby-vm-identity.py" after-stop "$identity" \
    > "$out/$label-cleanup-after.json" 2> "$out/$label-cleanup-after-error.txt" || verify_rc=$?
  printf 'stop=%s verify=%s\n' "$stop_rc" "$verify_rc" > "$out/$label-cleanup-status.txt"
  [ "$stop_rc" = 0 ] && [ "$verify_rc" = 0 ]
}
cleanup() {
  local rc=$? cleanup_rc=0
  trap - EXIT
  # Keep the original gate result and pre-stop evidence, including failures.
  printf '%s\n' "$rc" > "$out/gate-exit-status.txt" || cleanup_rc=1
  if [ -f "$out/verdict.txt" ]; then
    cp "$out/verdict.txt" "$out/gate-verdict.txt" || cleanup_rc=1
  fi
  collect a "$run_a" vm_a
  collect b "$run_b" vm_b
  stop_owned_vm a vm_a || cleanup_rc=1
  stop_owned_vm b vm_b || cleanup_rc=1
  # C boots for its phase only, which stops it when it passes.
  if [ -e "$out/c-boot-exit-status.txt" ] && [ ! -e "$out/c-cleanup-status.txt" ]; then
    collect c "$run_c" vm_c
    stop_owned_vm c vm_c || cleanup_rc=1
  fi
  printf '%s\n' "$cleanup_rc" > "$out/cleanup-exit-status.txt"
  if [ "$cleanup_rc" != 0 ]; then
    printf 'FAIL\n' > "$out/verdict.txt"
    note "normal cleanup of an owned VM failed; see cleanup evidence"
    [ "$rc" != 0 ] || rc=1
  fi
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  exit "$rc"
}
trap cleanup EXIT

# --- boot both machines -------------------------------------------------------
# The segment's hub lives as long as this script holds its stdin open.
exec {lan_hub_input}> >(exec "$vm" tool python3 "$dev_dir/nearby-lan-hub.py" "$lan_hub" 23901 23902 23903)
qemu-img create -f qcow2 -F qcow2 -b "$base" "$run_a/disk.qcow2" >/dev/null
qemu-img create -f qcow2 -F qcow2 -b "$base" "$run_b/disk.qcow2" >/dev/null
# --unlock grows B's disk once B has booted (os/dev/nearby-unlock-phase.sh):
# first boot grows the root partition over whatever disk it finds.
boot_owned_vm a "$run_a" vm_a
boot_owned_vm b "$run_b" vm_b
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

# The --wake and --unlock phases read what each session records: B's audit of
# a remote action, A's wake progress and unlock history, A's account of an idle
# hold. Those are info lines, which a session drops unless RUST_LOG asks for
# them. greetd starts the session with the PAM environment, which pam_env
# reads from /etc/environment, so both machines get it before their first
# login. Only the log filter changes.
if [ "$wake" = 1 ] || [ "$unlock" = 1 ]; then
  log_filter='error,tilo::remote_actions=info,tilo::device_wake=info,tilo::power=info,tilo_shell::device_unlock=info'
  for each_sudo in sudo_a sudo_b; do
    "$each_sudo" "set -eu
      sed -i '/^RUST_LOG=/d' /etc/environment
      printf '%s\n' 'RUST_LOG=$log_filter' >>/etc/environment" \
      || die "$each_sudo: could not set the login environment's log filter"
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
# A session the gate drives over ssh for minutes without touching its keyboard
# locks after its idle timeout like any other. A program in it holds an idle
# block lock as the person, the way a presentation tool does. The session
# honours it; the lock policy and its timeout stay as they are.
hold_awake() {
  local each=$1
  "$each" ssh "systemd-run --user --quiet --unit=tilo-nearby-idle-hold \
      systemd-inhibit --what=idle --mode=block --who='nearby pairing gate' \
        --why='drives this session over its control socket' sleep infinity" \
    || die "$each could not take an idle lock for the rest of the run"
  for attempt in $(seq 1 60); do
    session_log "$each" | grep -F 'kept awake by another program' | grep -F 'nearby pairing gate' \
      > "$out/$each-idle-hold.txt" && break
    [ "$attempt" -lt 60 ] || {
      "$each" ssh 'systemd-inhibit --list --no-pager' > "$out/$each-inhibitors.txt" 2>&1 || true
      die "$each's session never counted the idle lock; see $each-inhibitors.txt"
    }
    sleep 2
  done
  note "$each's session is held awake for the rest of the run"
}

# Each waits at onboarding while the gate drives the other, A while B signs in,
# B while A creates its identity, and then for the rest of the run.
wait_onboarding vm_a
hold_awake vm_a
wait_onboarding vm_b
hold_awake vm_b

# A: the ordinary Create path, exactly as the release gate makes it.
vm_a test native-atspi-smoke.sh oobe-actions
for attempt in $(seq 1 60); do
  vm_a ssh "test -s $identity_dir/primary.json && test -s /var/lib/tilo/device/cert.bin" && break
  [ "$attempt" -lt 60 ] || die "VM A did not create its identity"
  sleep 1
done
# The ready screen is finished with the keyboard: Tab reaches its primary
# action once the screen holds focus, and Return takes it.
vm_a test native-atspi-smoke.sh oobe-ready
vm_a keys tab
vm_a test native-atspi-smoke.sh oobe-finish-focused
vm_a keys ret
vm_a test native-atspi-smoke.sh oobe-finished
identity="$(vm_a ssh 'od -An -v -tx1 /var/lib/tilo/user-identity/pubkey.bin' | tr -d ' \n')"
[[ $identity =~ ^[0-9a-f]{64}$ ]] || die "VM A has no readable identity public key"
note "VM A holds identity $identity"


# --- the image owns the enrollment layout --------------------------------------
layout="$(vm_b ssh 'stat -c "%n %a %U %G" /var/lib/tilo/enrollment-staging /var/lib/tilo/enrollment-commits /var/lib/tilo/peers')"
printf '%s\n' "$layout" > "$out/b-enrollment-layout.txt"
for want in "enrollment-staging 3770" "enrollment-commits 3770" "peers 2770"; do
  dir=${want% *} mode=${want#* }
  grep -qx "/var/lib/tilo/$dir $mode root tilo-identity" <<<"$layout" \
    || die "/var/lib/tilo/$dir is not $mode root:tilo-identity on a fresh install: $layout"
done
vm_b ssh "id -nG | tr ' ' '\n' | grep -qx tilo-identity" \
  || die "the installed account is not in the device-trust group"
# The device key belongs to the image's first boot; joining must not touch it.
keypair_before="$(vm_b ssh 'stat -c "%a %U %G" /var/lib/tilo/device-private/keypair.bin')"
[[ $keypair_before == "640 "*" tilo-identity" ]] \
  || die "B's device key is not 0640 in the device-trust group on a fresh install: $keypair_before"

# --- one attempt: B asks, A approves -------------------------------------------
# Prints the pairing code the joining machine ($1, B unless named) published
# once it is waiting for approval.
await_code() {
  local each=${1:-vm_b} status code
  for attempt in $(seq 1 120); do
    status="$("$each" ssh "cat $status_file" 2>/dev/null || true)"
    if [ "$(jq -r '.state // empty' <<<"$status" 2>/dev/null)" = waiting_for_approval ]; then
      code="$(jq -r '.verification_code // empty' <<<"$status")"
      [[ $code =~ ^[0-9]{6}$ ]] || die "$each's status file carries no six-digit code: $status"
      printf '%s\n' "$code"
      return 0
    fi
    sleep 1
  done
  # What its screen says instead: the code it should show, or why not.
  "$each" test native-atspi-smoke.sh oobe-text "Verification code" >&2 || true
  die "$each never reported waiting_for_approval: ${status:-no status file}"
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
  local wanted=$1 seconds=$2 each=${3:-vm_b} status
  for _ in $(seq 1 "$seconds"); do
    status="$("$each" ssh "cat $status_file" 2>/dev/null || true)"
    if [ "$(jq -r '.state // empty' <<<"$status" 2>/dev/null)" = "$wanted" ]; then
      printf '%s\n' "$status"
      return 0
    fi
    sleep 1
  done
  die "$each's status never reached $wanted: ${status:-no status file}"
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
sudo_b 'chmod 3750 /var/lib/tilo/enrollment-commits'
vm_b test native-atspi-smoke.sh oobe-nearby
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
# Neither machine records a device whose enrollment never committed.
personal_devices vm_b > "$out/b-personal-devices-refused.json"
jq -e 'length == 0' "$out/b-personal-devices-refused.json" >/dev/null \
  || die "a failed pairing recorded personal devices on B: $(cat "$out/b-personal-devices-refused.json")"
if [ -s "$out/refused-device.txt" ]; then
  personal_devices vm_a > "$out/a-personal-devices-refused.json"
  jq -e --arg device "$(cat "$out/refused-device.txt")" 'index($device) == null' \
    "$out/a-personal-devices-refused.json" >/dev/null \
    || die "A recorded B as a personal device for a pairing B never committed"
fi
no_privileged_pairing
vm_b shot negative-error >/dev/null && cp -f "$run_b/negative-error.png" "$out/"

# --- retry: the same UI pairs -----------------------------------------------------
sudo_b 'chmod 3770 /var/lib/tilo/enrollment-commits'
vm_b test native-atspi-smoke.sh oobe-nearby
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
# Both machines record each other, and themselves, as the person's devices:
# without that, A's remote actions and wake requests refuse B as not enrolled.
a_device="$(vm_a ssh 'tilo-enroll device-key' | jq -r .device_public_key)"
[[ $a_device =~ ^[0-9a-f]{64}$ ]] || die "VM A did not report its device key"
for each in vm_a vm_b; do
  personal_devices "$each" > "$out/$each-personal-devices.json"
  jq -e --arg a "$a_device" --arg b "$device" '(sort) == ([$a, $b] | sort)' \
    "$out/$each-personal-devices.json" >/dev/null \
    || die "$each does not record exactly A and B as personal devices: $(cat "$out/$each-personal-devices.json")"
done

vm_b test native-atspi-smoke.sh oobe-nearby-done
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
# find's %g is the group's name (%G would be its number, which never equals
# a name), as stat's %G above is.
joined="$(vm_b ssh 'find /var/lib/tilo/device/cert.bin /var/lib/tilo/device/certs /var/lib/tilo/user-identity/pubkey.bin /var/lib/tilo/peers/personal-device-peers.json -printf "%p %y %g\n"')"
printf '%s\n' "$joined" > "$out/b-joined-files.txt"
grep -q '^/var/lib/tilo/device/certs d ' <<<"$joined" || die "the join wrote no per-identity certificate directory: $joined"
grep -q '^/var/lib/tilo/device/certs/.* f ' <<<"$joined" || die "the join wrote no per-identity certificate: $joined"
if awk '$3 != "tilo-identity"' <<<"$joined" | grep -q .; then
  die "the join wrote files outside the device-trust group: $joined"
fi
# Read as the session's own account, through the group, not as their owner.
sudo_b "set -eu
  find /var/lib/tilo/device/cert.bin /var/lib/tilo/device/certs /var/lib/tilo/user-identity/pubkey.bin \
    /var/lib/tilo/peers/personal-device-peers.json /var/lib/tilo/device-private/keypair.bin -type f \
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
vm_b test native-atspi-smoke.sh post-identity
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

# --- a device without the key pairs a third, approved on the key holder ------
qemu-img create -f qcow2 -F qcow2 -b "$base" "$run_c/disk.qcow2" >/dev/null
boot_owned_vm c "$run_c" vm_c
vm_c wait
for attempt in $(seq 1 60); do
  vm_c ssh true >/dev/null 2>&1 && break
  [ "$attempt" -lt 60 ] || die "vm_c: no SSH after boot"
  sleep 2
done
configure_lan sudo_c "$mac_c" "$lan_c" || die "VM C: shared segment could not be configured"
wait_onboarding vm_c
c_device="$(vm_c ssh 'tilo-enroll device-key' | jq -r .device_public_key)"
[[ $c_device =~ ^[0-9a-f]{64}$ ]] || die "VM C did not report its device key"
vm_c ssh "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/\$(id -u)/bus \
  tilo-identity pair --label 'Third device' --json" > "$out/c-pair.json" 2> "$out/c-pair.txt" &
c_pair=$!
code="$(await_code vm_c)"
vm_b ssh "tilo-identity approve --code $code --json" > "$out/b-approve-c.json" 2> "$out/b-approve-c.txt" &
b_approve=$!
# B's session asks A, the device holding the key; nobody else may let C in.
for attempt in $(seq 1 120); do
  vm_a ssh 'tilo-identity asking --json' > "$out/a-asking.json" 2>&1 || true
  jq -e --arg device "$c_device" 'any(.asking[]?; .device == $device)' "$out/a-asking.json" >/dev/null && break
  [ "$attempt" -lt 120 ] || die "A was never asked about C: $(cat "$out/a-asking.json")"
  sleep 1
done
jq -e --arg device "$c_device" --arg code "$code" \
  '[.asking[] | select(.device == $device)] | length == 1 and .[0].code == $code and .[0].label == "Third device"' \
  "$out/a-asking.json" >/dev/null || die "A's ask about C does not name it and its code: $(cat "$out/a-asking.json")"
[ "$(jq -r '.state // empty' < <(vm_c ssh "cat $status_file"))" = waiting_for_approval ] \
  || die "C joined before A approved it"
vm_c ssh '! test -e /var/lib/tilo/device/cert.bin' || die "C holds a certificate before A approved it"
vm_a ssh "tilo-identity approve --code $code --json" > "$out/a-approve-c.json" 2>&1 \
  || die "A could not approve C: $(cat "$out/a-approve-c.json")"
jq -e '.approved == true' "$out/a-approve-c.json" >/dev/null \
  || die "A answered C's ask with something else: $(cat "$out/a-approve-c.json")"
wait "$b_approve" || die "B's approval of C failed: $(cat "$out/b-approve-c.json" "$out/b-approve-c.txt")"
wait "$c_pair" || die "C's pairing failed: $(cat "$out/c-pair.json" "$out/c-pair.txt")"
complete="$(await_state complete 60 vm_c)"
printf '%s\n' "$complete" > "$out/c-status-complete.json"
[ "$(jq -r .identity <<<"$complete")" = "$identity" ] || die "C completed into another identity: $complete"
[ "$(vm_c ssh 'od -An -v -tx1 /var/lib/tilo/user-identity/pubkey.bin' | tr -d ' \n')" = "$identity" ] \
  || die "C's device is bound to another identity"
vm_b ssh "cat $identity_dir/fleets/$identity.json" > "$out/b-fleet-c.json" \
  || die "B has no fleet record for $identity"
device="$c_device" fleet_lists_b < "$out/b-fleet-c.json" || die "B's fleet record does not list C"
note "B paired C with A's approval"
collect c "$run_c" vm_c
stop_owned_vm c vm_c || die "C could not be stopped"

if [ "$wake" = 1 ]; then
  # shellcheck source=os/dev/nearby-wake-phase.sh
  source "$dev_dir/nearby-wake-phase.sh"
fi
if [ "$unlock" = 1 ]; then
  # shellcheck source=os/dev/nearby-unlock-phase.sh
  source "$dev_dir/nearby-unlock-phase.sh"
fi
printf 'PASS\n' > "$out/verdict.txt"
