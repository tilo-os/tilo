#!/bin/bash
# Power the trial VM off from inside: it ignores libvirt's ACPI shutdown.
set -u
V="virsh -c qemu:///session"
ssh -p 2222 -o BatchMode=yes -o ConnectTimeout=5 root@127.0.0.1 systemctl poweroff 2>/dev/null
for i in $(seq 1 40); do
  $V domstate tilo-trial | grep -q 'shut off' && exit 0
  sleep 3
done
$V destroy tilo-trial
