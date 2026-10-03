#!/usr/bin/env bash
# Bounded CI evidence for a VM that failed before the greeter gate. Raw serial
# may carry secrets, so only selected boot markers reach the job log.
set -euo pipefail

run=${TILO_RUN:?TILO_RUN is required}
serial="$run/serial.log"
qemu="$run/qemu.log"
bytes() { if [ -f "$1" ]; then wc -c < "$1"; else echo 0; fi; }
printf '::warning::boot evidence: serial=%s bytes qemu=%s bytes\n' "$(bytes "$serial")" "$(bytes "$qemu")" >&2

if [ -s "$serial" ]; then
  echo '::group::bounded serial boot markers (credential-bearing lines omitted)' >&2
  tr -d '\000' < "$serial" \
    | grep -aiE 'UEFI|EFI|BdsDxe|Linux version|Kernel panic|dracut|systemd|Failed|error|Started|Reached target|greetd|Booting|initrd|No bootable' \
    | grep -aviE 'password|passphrase|token|secret|credential|authorized_keys|command line|cmdline|private key' \
    | cut -c 1-240 | tail -n 80 >&2 || true
  echo '::endgroup::' >&2
fi
if [ -s "$qemu" ]; then
  echo '::group::bounded QEMU error markers (credential-bearing lines omitted)' >&2
  grep -aiE 'failed|error|warning|romfile|KVM|TCG' "$qemu" \
    | grep -aviE 'password|passphrase|token|secret|credential|authorized_keys|private key' \
    | cut -c 1-240 | tail -n 40 >&2 || true
  echo '::endgroup::' >&2
fi

# A display-none QEMU still has a virtio-gpu surface. Save one framebuffer
# before the boot step's cleanup stops the VM; CI uploads this only on failure.
if [ -S "$run/mon.sock" ]; then
  "$(dirname "${BASH_SOURCE[0]}")/vm.sh" cmd "screendump $run/boot-timeout.ppm" >/dev/null 2>&1 || true
fi
