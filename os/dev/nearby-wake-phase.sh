# shellcheck shell=bash
# The --wake phase of os/dev/test-nearby-pairing.sh, sourced once B has joined
# A's identity: the two paired devices reach and wake each other. It uses that
# script's vm_a/vm_b/sudo_b/die/note/session_log, $run_a, $out, $device (B's
# device id) and $mac_b.
#
#   5. A's remote action reaches B and holds B's idle-suspend off.
#   6. B suspends; A's `device.wake` puts a magic packet for B's MAC on the
#      segment; `system_wakeup` resumes B; A reports B rejoined, and A's next
#      remote action reaches B.
#   7. B's links are cut and restored; A's remote action reaches B again.

[ "$arch" = x86_64 ] || die "--wake needs x86_64: the ARM virt machine has no suspend-to-RAM"
vm_a ssh 'command -v tilo-control >/dev/null' \
  || die "--wake drives A through tilo-control, which only the dev disk carries"

# The person's own control socket on A, as the compositor binds it.
control_a() {
  vm_a ssh "tilo-control --socket /run/user/\$(id -u)/tilo-session/test-control.sock $(printf '%q ' "$@")"
}
# This boot's lines from B's session that match $1.
b_session_lines() {
  session_log vm_b | grep -cF -- "$1" || true
}
audit='remote action from another of your devices'
# A sends B one remote action until B's audit line count passes $1.
reach_b() {
  local before=$1 label=$2
  for attempt in $(seq 1 90); do
    # Any address reaches B's broker; B answers it, and records the visit
    # first whatever the answer.
    control_a actions invoke workspace $'workspace\x1flist' --device "$device" \
      >>"$out/wake-$label-invoke.txt" 2>&1 || true
    if [ "$(b_session_lines "$audit")" -gt "$before" ]; then
      note "A's remote action reached B ($label)"
      return 0
    fi
    sleep 2
  done
  die "A's remote action never reached B ($label); see wake-$label-invoke.txt"
}
qemu_status() { "$1" cmd 'info status' 2>/dev/null | tr -d '\r' | grep -o 'VM status: [a-z-]*' | tail -1; }
await_status() {
  local each=$1 wanted=$2 seconds=$3
  for _ in $(seq 1 "$seconds"); do
    [ "$(qemu_status "$each")" = "VM status: $wanted" ] && return 0
    sleep 1
  done
  die "$each never reached '$wanted' (last: $(qemu_status "$each"))"
}
await_ssh_b() {
  for attempt in $(seq 1 90); do
    vm_b ssh true >/dev/null 2>&1 && return 0
    sleep 2
  done
  die "B did not answer SSH after $1"
}

# --- 5. remote use holds B awake -----------------------------------------------
reach_b "$(b_session_lines "$audit")" first
for attempt in $(seq 1 20); do
  vm_b ssh 'systemd-inhibit --list --no-pager' > "$out/b-inhibitors.txt" 2>&1 || true
  grep -q 'Remote use from' "$out/b-inhibitors.txt" && break
  [ "$attempt" -lt 20 ] || die "B holds no 'Remote use from' sleep lock: $(cat "$out/b-inhibitors.txt")"
  sleep 1
done
note "B's idle-suspend is held off by A's remote use"

# B advertises its segment NIC as a wake route: e1000 with magic-packet wake.
vm_b ssh "cat /sys/class/net/*/address" | grep -qx "$mac_b" || die "B has no NIC with $mac_b"

# --- 6. B sleeps, A wakes it -----------------------------------------------------
# The person's own lease would hold suspend off; this is the person suspending
# anyway, as a closed lid would.
sudo_b 'systemd-run --on-active=2 --unit=tilo-wake-test-suspend systemctl suspend --check-inhibitors=no' \
  || die "B could not schedule its suspend"
await_status vm_b suspended 60
note "B is suspended"

# Capture A's side of the segment from here on.
pcap="$run_a/wake.pcap"
rm -f -- "$pcap"
vm_a cmd "object_add filter-dump,id=wakedump,netdev=tilolan,file=$pcap" >/dev/null
wanted="ffffffffffff$(for _ in $(seq 1 16); do printf '%s' "${mac_b//:/}"; done)"
woken=0
for attempt in $(seq 1 60); do
  if control_a actions invoke continuity.devices device.wake \
      --args-json "{\"device\":\"$device\"}" >>"$out/wake-invoke.txt" 2>&1; then
    woken=1
    break
  fi
  sleep 2
done
[ "$woken" = 1 ] || die "A's device.wake for B never ran: $(tail -5 "$out/wake-invoke.txt")"
for attempt in $(seq 1 30); do
  if [ -s "$pcap" ] && od -An -v -tx1 "$pcap" | tr -d ' \n' | grep -q "$wanted"; then
    break
  fi
  [ "$attempt" -lt 30 ] || die "no magic packet for $mac_b crossed the segment"
  sleep 1
done
vm_a cmd 'object_del wakedump' >/dev/null || true
cp -f "$pcap" "$out/wake.pcap"
note "A sent a magic packet for $mac_b"
[ "$(qemu_status vm_b)" = 'VM status: suspended' ] || die "B woke before the harness resumed it"

# QEMU's NICs do not implement Wake-on-LAN; the harness resumes B for them.
vm_b cmd system_wakeup >/dev/null
await_status vm_b running 30
await_ssh_b resume
for attempt in $(seq 1 90); do
  session_log vm_a | grep 'wake request progressed' > "$out/a-wake-progress.txt" || true
  grep -q 'Rejoined' "$out/a-wake-progress.txt" && break
  [ "$attempt" -lt 90 ] || die "A never saw B rejoin: $(cat "$out/a-wake-progress.txt")"
  sleep 2
done
note "A reports that B rejoined"
reach_b "$(b_session_lines "$audit")" after-wake

# --- 7. the route survives a cut link --------------------------------------------
# `info network` lists each NIC, then its backend as ` \ NAME: index=…,type=…`.
links="$(vm_b cmd 'info network' | tr -d '\r' \
  | sed -n 's/^[[:space:]]*\\[[:space:]]*\([^:[:space:]]*\): .*type=\(user\|socket\).*/\1/p')"
printf '%s\n' "$links" > "$out/b-links.txt"
grep -qx tilolan <<<"$links" || die "B's segment link is not named: $links"
for link in $links; do vm_b cmd "set_link $link off" >/dev/null; done
# Long enough for the peer connection to time out.
sleep 45
for link in $links; do vm_b cmd "set_link $link on" >/dev/null; done
await_ssh_b "its links came back"
reach_b "$(b_session_lines "$audit")" after-link-cut
note "A's remote action reaches B again after its links were cut"
