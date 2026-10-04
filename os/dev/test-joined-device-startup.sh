#!/usr/bin/env bash
# A device that joined an existing identity from a nearby device signs in
# straight to its desktop.
#
# Nearby pairing leaves the device a public-only identity record and an
# ordinary member certificate: it holds no root secret and does not read every
# workspace. This boots a throwaway overlay of an installed disk, joins it to a
# fresh identity through tilo-enroll's own personal-device commit (the
# out-of-band install-cert path stages and commits exactly what an approved
# pairing does, as root, like the pairing helper), selects the joined record as
# the account's identity the way a completed pairing leaves it, then signs in.
#
# The run proves:
#   1. The compositor starts the desktop, with neither identity setup nor
#      "Your existing identity needs recovery" in front of it.
#   2. The read-only identity gate the session wrapper runs at login passes.
#   3. Startup never minted a second identity or rewrote the enrollment: no
#      primary record, no root key on disk, the device certificate unchanged.
#   4. An authored app (Notes) opens, and its launch leaves the public record
#      as it was rather than giving it a secret it does not have.
#
# Usage: os/dev/test-joined-device-startup.sh DISK_QCOW2 [--out DIR] [--display gl|2d]
#
# DISK_QCOW2 is never booted or modified.
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

export TILO_USER="${TILO_USER:-tester}"
export TILO_PASS="${TILO_PASS:-tilo}"
arch="${TILO_VM_ARCH:-$(uname -m)}"
# Short: QEMU's monitor and console sockets live here (108-byte AF_UNIX limit).
export TILO_RUN="${TILO_JOINED_RUN:-/var/tmp/tilo-joined-$arch}"
export TILO_SSH_PORT="${TILO_JOINED_SSH_PORT:-2233}"
identity_dir='$HOME/.config/tilo/identity'

die() { printf 'joined device: %s\n' "$*" >&2; exit 1; }
note() { printf 'joined device: %s\n' "$*" >&2; }

mkdir -p "$out"
rm -rf -- "$TILO_RUN"
install -d -m 0700 "$TILO_RUN"

collect() {
  [ ! -f "$TILO_RUN/serial.log" ] || cp -f "$TILO_RUN/serial.log" "$out/serial.log"
  [ ! -f "$TILO_RUN/qemu.log" ] || cp -f "$TILO_RUN/qemu.log" "$out/qemu.log"
  "$vm" ssh 'journalctl -b --no-pager -o short-monotonic -n 600 _UID=$(id -u)' \
    > "$out/session-journal.txt" 2>&1 || true
  "$vm" ssh 'systemctl --failed --no-pager; systemctl --user --failed --no-pager' \
    > "$out/failed-units.txt" 2>&1 || true
  "$vm" ssh "ls -la $identity_dir /var/lib/tilo /var/lib/tilo/device /var/lib/tilo/device/certs \
    /var/lib/tilo/device-private /var/lib/tilo/user-identity; id" \
    > "$out/identity-layout.txt" 2>&1 || true
  "$vm" shot joined-final >/dev/null 2>&1 \
    && cp -f "$TILO_RUN/joined-final.png" "$out/" 2>/dev/null || true
}
cleanup() {
  local rc=$?
  trap - EXIT
  [ "$rc" -eq 0 ] || collect
  "$vm" stop >/dev/null 2>&1 || true
  rm -f -- "$TILO_RUN/disk.qcow2"
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  exit "$rc"
}
trap cleanup EXIT

"$vm" tool qemu-img create -f qcow2 -F qcow2 -b "$base" "$TILO_RUN/disk.qcow2" >/dev/null
"$vm" boot "--$display" --disk "$TILO_RUN/disk.qcow2"
"$vm" wait
for attempt in $(seq 1 60); do
  "$vm" ssh true >/dev/null 2>&1 && break
  [ "$attempt" -lt 60 ] || die "no SSH after boot"
  sleep 2
done
for attempt in $(seq 1 60); do
  "$vm" ssh 'test -s /var/lib/tilo/device-private/keypair.bin' && break
  [ "$attempt" -lt 60 ] || die "the device key was never created"
  sleep 1
done

# --- join an identity, as an approved nearby pairing does ----------------------
# The identity's root key stays on the approving side in a real pairing; here
# it exists only long enough to sign this device's member certificate and is
# removed before the device signs in.
cat > "$TILO_RUN/join.sh" <<'GUEST'
set -euo pipefail
pass=$1
identity_dir="$HOME/.config/tilo/identity"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
umask 077
install -d -m 0700 "$HOME/.config" "$HOME/.config/tilo" "$identity_dir"
head -c 32 /dev/urandom > "$work/root.key"
device="$(tilo-enroll device-key | sed -n 's/.*"device_public_key": *"\([0-9a-f]\{64\}\)".*/\1/p')"
[ -n "$device" ] || { echo "tilo-enroll printed no device key" >&2; exit 1; }
# `own` alone is the member certificate an approved personal device receives.
tilo-enroll certify --root-key "$work/root.key" --device "$device" \
  --label "Paired laptop" --capability own --out "$work/member.cert"
installed="$(printf '%s\n' "$pass" | sudo -S -p '' \
  tilo-enroll install-cert --identity-dir "$identity_dir" --cert "$work/member.cert")"
record="${installed#installed }"
[ -s "$record" ] || { echo "install-cert named no identity record: $installed" >&2; exit 1; }
printf '%s' "$(basename "$record" .json)" > "$identity_dir/active"
chmod 0600 "$identity_dir/active"
printf '%s\n' "$record"
GUEST
"$vm" ssh 'cat > /tmp/tilo-join.sh' < "$TILO_RUN/join.sh"
record="$("$vm" ssh "bash /tmp/tilo-join.sh $(printf '%q' "$TILO_PASS"); rm -f /tmp/tilo-join.sh")" \
  || die "the device could not be joined to an identity"
note "joined record $record"
"$vm" ssh "test ! -e $identity_dir/primary.json && test ! -e /var/lib/tilo/user-identity/privkey.bin" \
  || die "joining left a root identity on the device"
cert_before="$("$vm" ssh 'sha256sum /var/lib/tilo/device/cert.bin')"
"$vm" ssh "ls -la $identity_dir /var/lib/tilo/device /var/lib/tilo/device-private /var/lib/tilo/user-identity" \
  > "$out/joined-layout.txt" 2>&1 || true

# --- sign in ----------------------------------------------------------------------
"$vm" login --passcode-only "$TILO_PASS"
for attempt in $(seq 1 120); do
  "$vm" ssh 'busctl --user status org.freedesktop.impl.portal.desktop.tilo >/dev/null 2>&1' && break
  [ "$attempt" -lt 120 ] || die "the compositor did not start for the joined device"
  sleep 1
done
"$vm" ssh /usr/libexec/tilo/native-atspi-smoke.sh established \
  || die "the joined device did not sign in to its desktop"
"$vm" shot joined-desktop >/dev/null 2>&1 \
  && cp -f "$TILO_RUN/joined-desktop.png" "$out/" 2>/dev/null || true

# The identity chain check the release gate runs on an installed session, in
# the person's own session so their secret store answers.
"$vm" ssh 'DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus /usr/bin/shell --verify-collaboration-identity' \
  > "$out/verify-identity.txt" 2>&1 || {
    cat "$out/verify-identity.txt" >&2
    die "the joined identity does not verify at login"
  }
if "$vm" ssh 'journalctl -b --no-pager -o cat _UID=$(id -u)' \
    | grep -E 'needs recovery|identity secret has an invalid size|pre-enrolled device identity is not usable|refusing authored launch'; then
  die "the session treated the joined identity as one that needs recovery"
fi
"$vm" ssh "test ! -e $identity_dir/primary.json && test ! -e /var/lib/tilo/user-identity/privkey.bin" \
  || die "startup minted a root identity on a joined device"
[ "$("$vm" ssh 'sha256sum /var/lib/tilo/device/cert.bin')" = "$cert_before" ] \
  || die "startup replaced the joined device's certificate"
# An authored app starts on the joined device: its launch brokers the joined
# identity, which holds no key of its own.
"$vm" ssh '/usr/libexec/tilo/installed-atspi-apps.sh open Notes' > "$out/notes-open.txt" 2>&1 || {
  cat "$out/notes-open.txt" >&2
  die "Notes did not open on the joined device"
}
"$vm" shot joined-notes >/dev/null 2>&1 \
  && cp -f "$TILO_RUN/joined-notes.png" "$out/" 2>/dev/null || true
"$vm" ssh "grep -q '\"schema_version\": 1' $(printf '%q' "$record")" \
  || die "launching an app rewrote the joined identity record to reference a secret"
# Still the desktop once startup settles: nothing replaced it afterwards.
"$vm" ssh /usr/libexec/tilo/native-atspi-smoke.sh established \
  || die "the joined device's desktop did not stay up"
printf 'PASS\n' > "$out/verdict.txt"
note "a nearby-paired member device signs in to its desktop"
