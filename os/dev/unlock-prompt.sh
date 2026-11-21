#!/usr/bin/env bash
# Answer an installed guest's disk-unlock prompt from outside the guest.
#
#   ./os/dev/unlock-prompt.sh [SECRET]
#
# Without an argument the secret comes from TILO_UNLOCK_SECRET, which is how a
# harness passes it without it appearing on a command line.
#
# A VM whose installed root is LUKS2 stops in its initramfs until a secret is
# typed, and nothing is reachable over the network there. The prompt is written
# to /dev/console, which is where the kernel's last console= argument points, so
# it lands on the serial line the guest is booted with (os/dev/vm.sh boot
# --serial-socket serves it): the secret is sent there, as a person at the
# serial console would type it.
#
# It sends blindly, like os/dev/test-encrypted-install.sh: the prompt's arrival
# is not observable from here, and a secret sent slightly early stays in the
# tty's line buffer until the prompt reads it. Callers repeat it (the
# boot-assessment harness's UNLOCK does, every few seconds) and stop once the
# guest answers. Each call also keeps what the guest said in TILO_CONSOLE_LOG.
#
# Once this boot has reached a login prompt the volume is open, and a secret
# sent there becomes part of a login attempt and breaks the harness's own
# sign-in. So what this boot printed is read first (`vm.sh this-boot`): a login
# prompt in it means do nothing. Not TILO_CONSOLE_LOG: it holds only what a
# reader was connected for, and a `quiet` boot prints no kernel banner to tell
# its boots apart, so an earlier boot's prompt would stop every secret.
#
# A serial connection that cannot be made means another reader has the socket;
# the caller tries again. Without a socket (a harness that boots no serial
# console) the secret is typed on the emulated keyboard, which only reaches the
# prompt when the guest puts /dev/console on the screen.
#
# TILO_UNLOCK_SECRET, TILO_CONSOLE_LOG, TILO_SERIAL_SOCKET and the harness's
# variables come with it, as for every vm.sh action. TILO_UNLOCK_DELAY is how many
# seconds a call waits for the boot to reach its prompt before sending.
set -euo pipefail
secret=${1:-${TILO_UNLOCK_SECRET:-}}
[ -n "$secret" ] || { echo 'unlock-prompt: no secret (argument or TILO_UNLOCK_SECRET)' >&2; exit 2; }
log=${TILO_CONSOLE_LOG:-}
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "$("$here/vm.sh" this-boot 2>/dev/null || true)" in
  *login:*) exit 0 ;;
esac
socket=${TILO_SERIAL_SOCKET:-}
if [ -n "$socket" ] && [ -S "$socket" ]; then
  exec python3 -I "$here/serial-console.py" "$socket" \
    --log "${log:-/dev/null}" \
    "timeout:10" "sleep:${TILO_UNLOCK_DELAY:-20}" "send:$secret"
fi
printf '%s' "$secret" | "$here/vm.sh" type - >/dev/null 2>&1 || exit 1
"$here/vm.sh" keys ret >/dev/null 2>&1 || true
