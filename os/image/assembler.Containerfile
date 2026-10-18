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
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo \
    dnf install -y --setopt=reposdir=/repo --setopt=install_weak_deps=False \
      podman skopeo fuse-overlayfs e2fsprogs util-linux jq python3 git-core qemu-img \
    && dnf clean all \
    && podman --version && skopeo --version
