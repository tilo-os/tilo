#!/bin/bash
# On a fresh host, as the `tilo` user: download a published Tilo dev disk,
# put the VM on main's newest dev image and define + start `tilo-trial`.
#
#   FORGE_TOKEN=... ./create-vm.sh [disk-tag]     (default main-7997e2d-ci)
#
# The disk download needs a code.tilo.so token with package read access; the
# image itself comes from the public registry.tilo.so.
set -euo pipefail
tag="${1:-main-7997e2d-ci}"
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p ~/vm && cd ~/vm
base="https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk/$tag"
if [ ! -f base.qcow2 ]; then
  curl -sfL -H "Authorization: token ${FORGE_TOKEN:?set FORGE_TOKEN}" -o base.qcow2 "$base/tilo-vm-$tag-x86_64.qcow2"
  want=$(curl -sfL -H "Authorization: token $FORGE_TOKEN" "$base/tilo-vm-$tag-x86_64.qcow2.sha256" | awk '{print $1}')
  [ "$(sha256sum base.qcow2 | awk '{print $1}')" = "$want" ] || { echo "checksum mismatch" >&2; exit 1; }
fi
[ -f ~/.ssh/id_ed25519 ] || ssh-keygen -q -t ed25519 -N '' -f ~/.ssh/id_ed25519
cat ~/.ssh/authorized_keys ~/.ssh/id_ed25519.pub > root_keys
[ -f trial.qcow2 ] || qemu-img create -q -f qcow2 -F qcow2 -b base.qcow2 trial.qcow2 96G
sed "s|@ROOT_KEYS@|$(base64 -w0 root_keys)|" "$here/trial.xml.in" > trial.xml
install -m 0755 "$here/up.sh" "$here/down.sh" ~/vm/
mkdir -p ~/.config/systemd/user
install -m 0644 "$here/tilo-trial-vm.service" ~/.config/systemd/user/
virsh -c qemu:///session define trial.xml
virsh -c qemu:///session start tilo-trial
for i in $(seq 1 60); do
  ssh -p 2222 -o StrictHostKeyChecking=accept-new -o ConnectTimeout=5 -o BatchMode=yes root@127.0.0.1 true 2>/dev/null && break
  sleep 5
done
# The published disk lags; move onto the dev channel's newest image.
ssh -p 2222 root@127.0.0.1 'bootc switch registry.tilo.so/tilo/tilo-vm:x86_64-ci && systemctl reboot' || true
echo "VM defined; after its reboot run ~/vm/up.sh, then finish onboarding (see README)."
systemctl --user daemon-reload
systemctl --user enable tilo-trial-vm.service
