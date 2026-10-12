#!/usr/bin/env bash
# Run inside an installed Fedora VM as the graphical user. This deliberately
# exercises socket activation and the shipped unit sandbox, not an unconfined
# `tilo-ush serve` process started by the test.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
# It restarts the user's tilo-ush, ending every terminal session it serves.
# A developer host that has the session packages installed has that unit too.
if ! systemd-detect-virt --quiet --vm; then
  echo 'ush systemd terminal VM acceptance: refusing to restart tilo-ush outside a VM' >&2
  exit 2
fi
unit=/usr/lib/systemd/user/tilo-ush.service
socket_unit=/usr/lib/systemd/user/tilo-ush.socket

test -f "$unit"
test -f "$socket_unit"
grep -Fqx 'ProtectHostname=private' "$unit"
grep -Fqx 'Environment=TILO_USH_BIN=/usr/libexec/tilo-ush' "$unit"
for incompatible in ProtectKernelTunables ProtectKernelModules ProtectKernelLogs; do
  ! grep -Eq "^${incompatible}=(yes|true|1)$" "$unit"
done

systemctl --user stop tilo-ush.service tilo-ush.socket
systemctl --user start tilo-ush.socket
test "$(systemctl --user show -p ActiveState --value tilo-ush.socket)" = active

python3 "$repo/os/tests/ush-systemd-terminal-probe.py"

test "$(systemctl --user show -p ActiveState --value tilo-ush.service)" = active
main_pid=$(systemctl --user show -p MainPID --value tilo-ush.service)
test "$main_pid" -gt 1
# A sysext overlay's executable dentry may have no pathname in the root.
# Compare the running ELF itself, not that misleading textual symlink target.
cmp --silent "/proc/$main_pid/exe" /usr/libexec/tilo-ush
systemctl --user show -p Environment --value tilo-ush.service | \
  grep -Eq '(^| )TILO_USH_BIN=/usr/libexec/tilo-ush( |$)'

printf 'ush systemd terminal VM acceptance: PASS\n'
