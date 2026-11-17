#!/usr/bin/env bash
# RUNNER for os/tests/boot_assessment_rollback.sh on a machine reached only at
# its serial console: the server image runs no sshd, so a check drives it the
# way a person at the console would.
#
#   TILO_SERIAL_SOCKET=$TILO_RUN/serial.sock TILO_CONSOLE_LOG=/out/console.log \
#   TILO_CONSOLE_USER=tester TILO_CONSOLE_PASS=tilo \
#     ./os/tests/serial_runner.sh 'systemctl is-active multi-user.target'
#
# One shell line in the signed-in session, its output on stdout and its status
# as this script's. TILO_CONSOLE_PROMPT_TIMEOUT bounds the wait for a prompt
# (20 s), TILO_CONSOLE_DEADLINE the wait for the command (600 s).
set -euo pipefail
: "${TILO_SERIAL_SOCKET:?set TILO_SERIAL_SOCKET to the serial socket of the guest}"
: "${TILO_CONSOLE_LOG:?set TILO_CONSOLE_LOG to the file the console is kept in}"
[ "$#" -ge 1 ] || { echo 'usage: serial_runner.sh LINE' >&2; exit 2; }
exec python3 -I "$(dirname -- "${BASH_SOURCE[0]}")/../dev/serial-console.py" "$TILO_SERIAL_SOCKET" \
  --log "$TILO_CONSOLE_LOG" --run "$1" \
  --login "${TILO_CONSOLE_USER:-tester}:${TILO_CONSOLE_PASS:-tilo}" \
  --timeout "${TILO_CONSOLE_PROMPT_TIMEOUT:-20}" \
  --deadline "${TILO_CONSOLE_DEADLINE:-600}"
