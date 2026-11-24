#!/usr/bin/env bash
# The session user's Tilo, as on a workstation: a person made here and that person's daemon,
# which tilo-vm adds its VMs to as devices. A micro-VM has no systemd user manager to run the
# daemon as the person's service, so it runs in the background for as long as the check does;
# its log goes to $1.
set -euo pipefail
tilo login --new-identity >/dev/null
setsid -f tilo-runtime >"$1" 2>&1 </dev/null
for _ in $(seq 150); do
  tilo device key >/dev/null 2>&1 && exit 0
  sleep 0.2
done
echo "the session user's Tilo daemon did not answer within 30s; its log:" >&2
cat "$1" >&2
exit 1
