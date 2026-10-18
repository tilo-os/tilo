# The OS assembler: the image t runs the OS image's layer actions in, as root in a
# micro-VM step (os/image/build-layer.sh). The pinned base with what building and
# verifying a layer takes, from the frozen RPM snapshot: podman builds the pieces,
# skopeo and jq read the pinned inputs, fuse-overlayfs is podman's storage where the
# guest's kernel overlay can't be, git names the commit, qemu-img compresses disks. Baked here once, so no layer
# action installs packages before it starts; rebuilt only when this file or the pins
# change.
ARG BASE_REF
ARG RPMS_REF
FROM ${RPMS_REF} AS rpms

FROM ${BASE_REF}
# QEMU, its firmware and a software TPM boot the image's disk for VM tests and development VMs
# (os/image/dev-vm.sh), nested in the step's guest.
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo \
    case "$(uname -m)" in \
      x86_64) vm="qemu-system-x86-core edk2-ovmf qemu-device-display-virtio-vga" ;; \
      aarch64) vm="qemu-system-aarch64-core edk2-aarch64 qemu-device-display-virtio-gpu-pci" ;; \
    esac \
    && dnf install -y --setopt=reposdir=/repo --setopt=install_weak_deps=False \
      podman skopeo fuse-overlayfs e2fsprogs util-linux jq python3 git-core qemu-img \
      swtpm swtpm-tools curl zstd $vm \
    && dnf clean all \
    && podman --version && skopeo --version
