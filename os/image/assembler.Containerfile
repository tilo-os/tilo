# The OS assembler: the image t runs the OS image's layer actions in, as root in a
# micro-VM step (os/image/build-layer.sh). The pinned base with what building and
# verifying a layer takes, from the frozen RPM snapshot: podman builds the pieces,
# skopeo and jq read the pinned inputs, fuse-overlayfs is podman's storage where the
# guest's kernel overlay can't be, git names the commit. Baked here once, so no layer
# action installs packages before it starts; rebuilt only when this file or the pins
# change.
ARG BASE_REF
ARG RPMS_REF
FROM ${RPMS_REF} AS rpms

FROM ${BASE_REF}
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo \
    dnf install -y --setopt=reposdir=/repo --setopt=install_weak_deps=False \
      podman skopeo fuse-overlayfs e2fsprogs util-linux jq python3 git-core \
    && dnf clean all \
    && podman --version && skopeo --version
# Podman's overlay storage diffs a layer natively only where overlay renames no directory by
# redirect, which Fedora's default mount options (metacopy, which implies it) and a kernel that
# defaults redirect_dir on both turn on: every committed instruction then walks the whole tree
# below it, minutes per COPY over a multi-gigabyte rootfs.
RUN sed 's/^mountopt = .*/mountopt = "nodev,redirect_dir=off"/' /usr/share/containers/storage.conf \
      > /etc/containers/storage.conf \
    && grep -qx 'mountopt = "nodev,redirect_dir=off"' /etc/containers/storage.conf
# The base's /root is a link to /var/roothome, which a booted system makes at boot (tmpfiles) and
# an action's guest never does: root's home must exist for cosign's and podman's state.
RUN mkdir -p /var/roothome && test -d /root/
