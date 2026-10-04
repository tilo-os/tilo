#!/usr/bin/env bash
# Run scripts/legacy-vm-e2e.py inside the CI builder image.
#
#   legacy-vm-e2e.sh        (from the repository root, as root in the container)
#
# Expects e2e/disk/disk.qcow2 (the fresh guest disk), e2e/bin/{shell,tilo-legacy}
# and the runner's /dev/kvm; /dev/vhost-vsock when the runner's kernel has it,
# else the user-space vhost-device-vsock serves the VM's vsock. Installs the
# host side of a legacy VM, then runs the test as an ordinary user, whose
# libvirt session owns the VM. Logs and screenshots land in e2e/artifacts.
set -euo pipefail

test -c /dev/kvm
chmod 0666 /dev/kvm
[[ ! -c /dev/vhost-vsock ]] || chmod 0666 /dev/vhost-vsock

dnf install -y --setopt=install_weak_deps=False \
  libvirt-client libvirt-daemon-driver-qemu libvirt-daemon-common \
  qemu-kvm-core qemu-img edk2-ovmf passt virtiofsd waypipe skopeo vhost-device-vsock
command -v virsh virtqemud qemu-system-x86_64 passt waypipe >/dev/null
test -x /usr/libexec/virtiofsd

useradd --create-home legacy-e2e
chmod 0755 e2e/bin/shell e2e/bin/tilo-legacy
runuser -u legacy-e2e -- env PATH="$PATH" \
  WGPU_BACKEND=vulkan \
  VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
  WGPU_ADAPTER_NAME=llvmpipe \
  python3 scripts/legacy-vm-e2e.py \
    --shell "$PWD/e2e/bin/shell" \
    --tilo-legacy "$PWD/e2e/bin/tilo-legacy" \
    --image "$PWD/e2e/disk/disk.qcow2" \
    --artifacts "$PWD/e2e/artifacts"
