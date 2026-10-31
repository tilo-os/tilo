#!/bin/bash
# Start the trial VM, wait for its ssh, and sign quickuser in at the greeter.
# Runs on the host as the `tilo` user (tilo-trial-vm.service runs it at boot).
set -u
V="virsh -c qemu:///session"
$V domstate tilo-trial | grep -q running || $V start tilo-trial
for i in $(seq 1 60); do
  ssh -p 2222 -o ConnectTimeout=5 -o BatchMode=yes root@127.0.0.1 "systemctl is-active greetd" >/dev/null 2>&1 && break
  sleep 5
done
sleep 20
session_up() {
  ssh -p 2222 -o BatchMode=yes quickuser@127.0.0.1 "test -S /run/user/\$(id -u)/tilo-session/agent.sock" 2>/dev/null
}
if ! session_up; then
  for k in KEY_T KEY_I KEY_L KEY_O; do $V send-key tilo-trial "$k" >/dev/null; sleep 0.1; done
  $V send-key tilo-trial KEY_ENTER >/dev/null
fi
for i in $(seq 1 30); do
  session_up && { echo "trial VM ready"; exit 0; }
  sleep 3
done
echo "trial VM booted but the desktop session did not come up" >&2
exit 1
