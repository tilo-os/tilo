#!/usr/bin/env bash
# Boot a legacy guest disk once and wait until its agent serves vsock.
#
#   legacy-guest-smoke.sh DISK.qcow2
#
# The disk is never written: QEMU boots a throwaway overlay. Success is the
# agent's "serving vsock port" line on the serial console. Needs KVM and
# x86-64 OVMF; the caller checks KVM first (os/dev/probe-kvm.py).
set -euo pipefail

disk=$(realpath "$1")
work=$(mktemp -d "${RUNNER_TEMP:-/var/tmp}/legacy-guest-smoke.XXXXXX")
trap 'kill "${qemu:-}" 2>/dev/null || true; rm -rf -- "$work"' EXIT

code=
for candidate in /usr/share/edk2/ovmf/OVMF_CODE.fd /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd; do
  if [[ -r $candidate ]]; then code=$candidate; break; fi
done
[[ -n $code ]] || { echo "::error::no x86-64 OVMF firmware"; exit 1; }
vars_template=${code/CODE/VARS}
cp "$vars_template" "$work/vars.fd"

qemu-img create -q -f qcow2 -F qcow2 -b "$disk" "$work/overlay.qcow2"
vsock=()
if [[ -w /dev/vhost-vsock ]]; then
  vsock=(-device "vhost-vsock-pci,guest-cid=$((1000 + RANDOM))")
  echo "booting with a vhost-vsock device"
else
  echo "no /dev/vhost-vsock on this runner; booting without a vsock device"
fi
qemu-system-x86_64 \
  -machine q35,accel=kvm -cpu host -smp 2 -m 2048 \
  -drive "if=pflash,format=raw,readonly=on,file=$code" \
  -drive "if=pflash,format=raw,file=$work/vars.fd" \
  -drive "file=$work/overlay.qcow2,if=virtio,format=qcow2" \
  -nic user,model=virtio-net-pci \
  "${vsock[@]}" \
  -display none -serial "file:$work/serial.log" -monitor none &
qemu=$!

for _ in $(seq 1 300); do
  if grep -q 'tilo-legacy-agent: serving vsock port' "$work/serial.log" 2>/dev/null; then
    echo "the guest agent is serving"
    exit 0
  fi
  if ! kill -0 "$qemu" 2>/dev/null; then
    break
  fi
  sleep 1
done
echo "::error::the guest agent did not come up; serial console:"
tail -n 80 "$work/serial.log" || true
exit 1
