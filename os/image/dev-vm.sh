#!/usr/bin/env bash
# A development VM of the OS image: os-disk-ARCH booted under KVM, with a TPM, on a t node, as a
# job of the session that asked for it, attachable through t (`t vm console`, `t vm ssh`,
# `t vm display`) for its lifetime (`t vm extend` adds to it; `t vm stop` ends it).
#
#   dev-vm.sh ARCH DISK [LIFETIME_MINUTES]
#
# The disk is a copy-on-write overlay over os-disk-ARCH's, so the built disk stays as it was and
# what the developer does in the VM goes with the job. t's attach agent (served by the server
# that ran the job) runs QEMU and registers its channels: the serial console and the display as
# QEMU's unix sockets, SSH as the guest's port 22 forwarded inside the job. Nothing listens on
# the node's network. The VM counts against the session's share as the job it is: the cores and
# memory it booted with.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/../../tools/ci/publish/lib.sh"
cd "$repo_root"
arch=${1:?usage: dev-vm.sh ARCH DISK [LIFETIME_MINUTES]}
disk=${2:?}
lifetime=$(( ${3:-${T_INPUT_LIFETIME:-60}} * 60 ))
[[ $(uname -m) == "$arch" ]] || die "the $arch VM boots natively"
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check requires /dev/kvm'
[[ -n ${T_ATTACH_URL:-} && -n ${T_ATTACH_TOKEN:-} ]] \
  || die 'this t server gives jobs no attach token ([oidc] issuer, its public URL, is not set): the VM would be unreachable'

export TILO_VM_ARCH=$arch TILO_RUN=$scratch/dev-vm TILO_TPM=1 TILO_DISTROBOX=
. os/dev/_common.sh
mkdir -p "$TILO_RUN"
overlay=$TILO_RUN/disk.qcow2
qemu-img create -q -f qcow2 -F qcow2 -b "$(realpath "$disk")" "$overlay"
serial=$TILO_RUN/serial.sock vnc=$TILO_RUN/vnc.sock
curl -fsS -H "authorization: Bearer $T_ATTACH_TOKEN" "$T_ATTACH_URL/v1/attach-agent/agent.py" -o "$TILO_RUN/agent.py"

firmware_setup
varstore=$(firmware_varstore)
swtpm_start
# The guest's memory and cores are the job's: what the check declares, less QEMU's own.
mem=$(( $(awk '/^MemTotal:/ {print $2}' /proc/meminfo) / 1024 * 3 / 4 ))
smp=$(nproc)
read -r -a qemu_args <<< "$(machine_args) $(qemu_accel_args) -m $mem -smp $smp $(pflash_args "$varstore") \
  -drive file=$overlay,format=qcow2,if=none,id=tilo-disk -device virtio-blk-pci,drive=tilo-disk \
  -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:2222-:22 \
  $(display_qemu_args 2d | sed 's/-display none//') -vnc unix:$vnc $(input_qemu_args) \
  -serial unix:$serial,server=on,wait=off $(tpm_qemu_args) -name tilo-dev-vm"
case $arch in
  x86_64) qemu=qemu-system-x86_64 ;;
  aarch64) qemu=qemu-system-aarch64 ;;
esac
digest=$(cat "$(dirname "$disk")/image-digest" 2>/dev/null || echo unknown)
exec python3 -I "$TILO_RUN/agent.py" --label "os-vm-$arch of ${digest:0:19}" \
  --lifetime "$lifetime" --max "${T_VM_MAX_SECS:-43200}" \
  --console "unix:$serial" --ssh tcp:127.0.0.1:2222 --display "unix:$vnc" \
  -- "$qemu" "${qemu_args[@]}"
