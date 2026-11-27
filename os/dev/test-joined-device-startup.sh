#!/usr/bin/env bash
# A computer that joined a person's identity with a ticket from one of their
# devices signs in straight to its desktop.
#
# `tilo login` is the join: the device that issued the ticket certifies this
# computer's key through its daemon, and this computer enters the person's
# workspace holding none of the identity's root. This boots two throwaway
# overlays of an installed disk on one private LAN: A founds the person
# through its own daemon and issues a ticket from that founding; B spends the
# ticket and signs in at its greeter.
#
# The run proves:
#   1. The compositor starts B's desktop, with neither identity setup nor
#      "Your existing identity needs recovery" in front of it.
#   2. The read-only identity check the session runs at login passes.
#   3. The join and the startup minted no second identity and hold no root:
#      no primary record, no secret on disk, the device certificate unchanged.
#   4. An authored app (Notes) opens, and its launch leaves the identity
#      directory byte-for-byte as it was: the public record gains no secret it
#      does not have, and nothing else there is written.
#   5. A note typed there is saved, and Notes says "Saved".
#
# Usage: os/dev/test-joined-device-startup.sh DISK_QCOW2 [--out DIR] [--display gl|2d]
#
# DISK_QCOW2 is never booted or modified; each VM boots its own overlay.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
vm="$dev_dir/vm.sh"
base="${1:?usage: test-joined-device-startup.sh DISK_QCOW2 [--out DIR] [--display gl|2d]}"
shift
out="$PWD/joined-device-artifacts"
display="${TILO_VM_DISPLAY:-gl}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --display) display="${2:?--display needs gl or 2d}"; shift 2 ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
case "$display" in gl|2d) ;; *) echo '--display must be gl or 2d' >&2; exit 2 ;; esac
[ -f "$base" ] || { printf 'disk not found: %s\n' "$base" >&2; exit 1; }
base="$(cd "$(dirname "$base")" && pwd)/$(basename "$base")"
command -v jq >/dev/null || { echo 'jq is required' >&2; exit 1; }

export TILO_USER="${TILO_USER:-tester}"
export TILO_PASS="${TILO_PASS:-tilo}"
arch="${TILO_VM_ARCH:-$(uname -m)}"
# Short: QEMU's monitor and console sockets live here (108-byte AF_UNIX limit).
run_a="${TILO_JOINED_RUN_A:-/var/tmp/tilo-joined-a-$arch}"
run_b="${TILO_JOINED_RUN_B:-/var/tmp/tilo-joined-b-$arch}"
# The machine that founds the person and issues the ticket, and the machine
# that joins with it. Both NICs sit on one multicast segment, so the ticket's
# endpoint is reached directly, as it is on a home network; the dev image
# names each NIC and gives it 198.18.<5th>:<6th> from its MAC (tools/tilo-vm
# lan), so neither guest needs a hand-configured address.
lan_group="${TILO_JOINED_LAN_GROUP:-239.255.84.76:$((46000 + $$ % 19000))}"
mac_a=52:54:01:00:12:34
mac_b=52:54:01:00:12:35
identity_dir='$HOME/.config/tilo/identity'

die() { printf 'joined device: %s\n' "$*" >&2; exit 1; }
note() { printf 'joined device: %s\n' "$*" >&2; }

mkdir -p "$out"
rm -rf -- "$run_a" "$run_b"
install -d -m 0700 "$run_a" "$run_b"

vm_a() {
  TILO_RUN="$run_a" TILO_SSH_PORT="${TILO_JOINED_SSH_A:-2231}" \
    TILO_VM_LAN_GROUP="$lan_group:$mac_a" "$vm" "$@"
}
vm_b() {
  TILO_RUN="$run_b" TILO_SSH_PORT="${TILO_JOINED_SSH_B:-2232}" \
    TILO_VM_LAN_GROUP="$lan_group:$mac_b" "$vm" "$@"
}

collect() {
  local label=$1 run=$2 each=$3
  [ ! -f "$run/serial.log" ] || cp -f "$run/serial.log" "$out/$label-serial.log"
  [ ! -f "$run/qemu.log" ] || cp -f "$run/qemu.log" "$out/$label-qemu.log"
  "$each" ssh 'journalctl -b --no-pager -o short-monotonic -n 600 _UID=$(id -u)' \
    > "$out/$label-session-journal.txt" 2>&1 || true
  "$each" ssh 'systemctl --failed --no-pager; systemctl --user --failed --no-pager' \
    > "$out/$label-failed-units.txt" 2>&1 || true
  # The compositor logs to the session's state directory, not the journal; it
  # alone says why a session ended right after login.
  "$each" ssh 'cat "$HOME"/.local/state/tilo/session-logs/*.log' \
    > "$out/$label-compositor-session.log" 2>&1 || true
  "$each" ssh 'journalctl -k -b --no-pager -o short-monotonic -n 400' \
    > "$out/$label-kernel-journal.txt" 2>&1 || true
  "$each" ssh "ls -la $identity_dir /var/lib/tilo /var/lib/tilo/device /var/lib/tilo/device/certs \
    /var/lib/tilo/device-private /var/lib/tilo/user-identity; id" \
    > "$out/$label-identity-layout.txt" 2>&1 || true
  "$each" shot "$label-final" >/dev/null 2>&1 \
    && cp -f "$run/$label-final.png" "$out/" 2>/dev/null || true
}
cleanup() {
  local rc=$?
  trap - EXIT
  [ "$rc" -eq 0 ] || { collect a "$run_a" vm_a; collect b "$run_b" vm_b; }
  vm_a stop >/dev/null 2>&1 || true
  vm_b stop >/dev/null 2>&1 || true
  rm -f -- "$run_a/disk.qcow2" "$run_b/disk.qcow2"
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  exit "$rc"
}
trap cleanup EXIT

qemu-img create -f qcow2 -F qcow2 -b "$base" "$run_a/disk.qcow2" >/dev/null
qemu-img create -f qcow2 -F qcow2 -b "$base" "$run_b/disk.qcow2" >/dev/null
vm_a boot "--$display" --disk "$run_a/disk.qcow2"
vm_b boot "--$display" --disk "$run_b/disk.qcow2"
for each in vm_a vm_b; do
  for attempt in $(seq 1 90); do
    "$each" ssh true >/dev/null 2>&1 && break
    [ "$attempt" -lt 90 ] || die "$each: no SSH after boot"
    sleep 2
  done
done
for each in vm_a vm_b; do
  for attempt in $(seq 1 90); do
    "$each" ssh 'systemctl is-active --quiet tilo-runtime' >/dev/null 2>&1 && break
    [ "$attempt" -lt 90 ] || die "$each: the device daemon did not start"
    sleep 2
  done
done

# --- the person is founded on A, and issues B's ticket ------------------------
founding="$(TILO_SSH_BOUND=180 vm_a ssh 'tilo login --new-identity --name tester --json --ticket device')" \
  || die "A could not make the person its daemon holds"
printf '%s\n' "$founding" > "$out/founding.json"
ticket="$(printf '%s\n' "$founding" \
  | jq -r '[(.tickets // [])[] | select(.kind == "device") | .ticket][0] // empty')"
[ -n "$ticket" ] || die "founding the person issued no ticket for a computer"
case "$ticket" in tilo1*) ;; *) die "the issued ticket is not a Tilo ticket" ;; esac
note "the person is founded on A, which issued a ticket for B"

# --- B joins the person with it ----------------------------------------------
# `-` takes the ticket on standard input, so it never shows in a process list.
joined="$(printf '%s\n' "$ticket" | TILO_SSH_BOUND=300 vm_b ssh 'tilo login - --label "Joined laptop"')" \
  || die "B did not join the person's devices"
printf '%s\n' "$joined" > "$out/join.txt"
grep -q 'Logged in' <<<"$joined" || die "B joined without saying it logged in: ${joined: -1200}"
grep -q 'Entered' <<<"$joined" || die "B logged in but entered no workspace: ${joined: -1200}"
note "B joined the person with A's ticket"
vm_b ssh "test ! -e $identity_dir/primary.json && test ! -e $identity_dir/secrets" \
  || die "joining left a root identity on the device"
# The person reads the device certificate as themselves, as their session
# does to verify the device: it is public, whatever umask joining ran under.
cert_before="$(vm_b ssh 'sha256sum /var/lib/tilo/device/cert.bin')" \
  || die "joining left the device certificate unreadable to the person"
vm_b ssh 'set -- /var/lib/tilo/device/certs/*.bin && [ -e "$1" ] && cat -- "$@" >/dev/null' \
  || die "joining left the per-identity certificates unreadable to the person"
vm_b ssh "ls -la $identity_dir /var/lib/tilo/device /var/lib/tilo/device-private /var/lib/tilo/user-identity" \
  > "$out/joined-layout.txt" 2>&1 || true

# --- B signs in at its greeter ------------------------------------------------
vm_b wait
vm_b login --passcode-only "$TILO_PASS"
for attempt in $(seq 1 120); do
  vm_b ssh 'busctl --user status org.freedesktop.impl.portal.desktop.tilo >/dev/null 2>&1' && break
  [ "$attempt" -lt 120 ] || die "the compositor did not start for the joined device"
  sleep 1
done
vm_b test native-atspi-smoke.sh established \
  || die "the joined device did not sign in to its desktop"
vm_b shot joined-desktop >/dev/null 2>&1 \
  && cp -f "$run_b/joined-desktop.png" "$out/" 2>/dev/null || true

# The identity chain check the release check runs on an installed session, in
# the person's own session so their secret store answers.
vm_b ssh 'DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus /usr/bin/tilo-session --verify-collaboration-identity' \
  > "$out/verify-identity.txt" 2>&1 || {
    cat "$out/verify-identity.txt" >&2
    die "the joined identity does not verify at login"
  }
if vm_b ssh 'journalctl -b --no-pager -o cat _UID=$(id -u)' \
    | grep -E 'needs recovery|identity secret has an invalid size|pre-enrolled device identity is not usable|refusing authored launch'; then
  die "the session treated the joined identity as one that needs recovery"
fi
vm_b ssh "test ! -e $identity_dir/primary.json && test ! -e $identity_dir/secrets" \
  || die "startup minted a root identity on a joined device"
[ "$(vm_b ssh 'sha256sum /var/lib/tilo/device/cert.bin')" = "$cert_before" ] \
  || die "startup replaced the joined device's certificate"
# An authored app starts on the joined device: its launch brokers the joined
# identity, which holds no key of its own, and writes none of its records. The
# local index (registry.json) may record which identity the workspace it opens
# in uses.
identity_manifest="cd $identity_dir && find . -mindepth 1 ! -path ./registry.json -printf '%p %y %m %i %s %T@\n' | LC_ALL=C sort \
  && find . -type f ! -path ./registry.json -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum"
vm_b ssh "$identity_manifest" > "$out/identity-before-notes.txt" \
  || die "could not record the identity directory"
vm_b test installed-atspi-apps.sh 'open Notes' > "$out/notes-open.txt" 2>&1 || {
  cat "$out/notes-open.txt" >&2
  # A desktop that died under the launch leaves no app to find on the bus;
  # say that, with the status the session wrapper recorded, rather than
  # blame the app.
  session_end="$(vm_b ssh 'grep -a "^tilo-flight-entry-v1 end=" "$HOME/.local/state/tilo/session-logs/latest.log"' 2>/dev/null || true)"
  [ -z "$session_end" ] || die "the desktop session ended while Notes opened: $session_end"
  die "Notes did not open on the joined device"
}
vm_b ssh "$identity_manifest" > "$out/identity-after-notes.txt" \
  || die "could not record the identity directory"
diff -u "$out/identity-before-notes.txt" "$out/identity-after-notes.txt" >&2 \
  || die "opening Notes wrote the identity directory"
vm_b shot joined-notes >/dev/null 2>&1 \
  && cp -f "$run_b/joined-notes.png" "$out/" 2>/dev/null || true
# Close that Notes again: the next one opens from the empty workspace's
# launcher, as a person starting a note does.
notes_pid="$(awk 'NF == 2 && $1 ~ /^:/ && $2 ~ /^[0-9]+$/ { pid = $2 } END { print pid }' "$out/notes-open.txt")"
[[ "$notes_pid" =~ ^[0-9]+$ ]] || die "opening Notes named no process: $(cat "$out/notes-open.txt")"
vm_b ssh "kill $notes_pid; for _ in \$(seq 1 50); do kill -0 $notes_pid 2>/dev/null || exit 0; sleep 0.1; done; exit 1" \
  || die "the Notes the launch check opened did not close"
# A note written on the joined device is durable, and Notes says so: the
# keyboard types it, and the save state reads "Saved", never "Not saved".
joined_sentence='Written on the joined device.'
vm_b test installed-atspi-apps.sh 'type-into Notes "New note" --field "Block text" --state /tmp/joined-notes' \
  > "$out/notes-type.txt" 2>&1 || {
  cat "$out/notes-type.txt" >&2
  die "Notes on the joined device opened no note to type into"
}
vm_b type "$joined_sentence"
vm_b test installed-atspi-apps.sh "typed '$joined_sentence' --state /tmp/joined-notes --status Saved" \
  > "$out/notes-saved.txt" 2>&1 || {
  cat "$out/notes-saved.txt" >&2
  die "a note written on the joined device is not saved"
}
# Still the desktop once startup settles: nothing replaced it afterwards.
vm_b test native-atspi-smoke.sh established \
  || die "the joined device's desktop did not stay up"
printf 'PASS\n' > "$out/verdict.txt"
note "a computer that joined with a ticket signs in to its desktop"
