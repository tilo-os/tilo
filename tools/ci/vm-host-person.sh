#!/usr/bin/env bash
# The session user's Tilo, as on a workstation: a person made here and that person's daemon,
# which tilo-vm adds its VMs to as devices. A micro-VM has no systemd user manager to run the
# daemon as the person's service, so it runs in the background for as long as the check does;
# its log goes to $1.
set -euo pipefail
# The daemon first: this micro-VM has no systemd user manager, so the CLI cannot start one as
# the person's service and a login alone fails saying so. Running it here for as long as the
# check does leaves the login finding a daemon that answers, which is what founds the person.
setsid -f tilo-runtime >"$1" 2>&1 </dev/null
for _ in $(seq 150); do
  tilo device key >/dev/null 2>&1 && break
  sleep 0.2
done
if ! tilo device key >/dev/null 2>&1; then
  echo "the session user's Tilo daemon did not answer within 30s; its log:" >&2
  cat "$1" >&2
  exit 1
fi
tilo login --new-identity >/dev/null
