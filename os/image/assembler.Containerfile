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
# The same bytes from the same base and snapshot: t keys every OS action run in this image by
# them. rpm dates what it installs by SOURCE_DATE_EPOCH (the build's clock) instead of the wall
# clock, and the transaction leaves its database finalized and no diagnostic state (DNF's history
# and log, ldconfig's aux-cache, a machine ID), as the OS's packages layer does
# (os/image/packages.Containerfile); the scripts are the `scripts` build context, os/scripts.
ARG SOURCE_DATE_EPOCH=0
COPY --from=scripts --chmod=0755 finalize-rpmdb.sh /usr/libexec/tilo/finalize-rpmdb
COPY --from=scripts --chmod=0755 clean-dnf-layer-state.sh /usr/libexec/tilo/clean-dnf-layer-state
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo \
    dnf install -y --setopt=reposdir=/repo --setopt=install_weak_deps=False \
      podman skopeo fuse-overlayfs e2fsprogs util-linux jq python3 git-core \
    && dnf clean all \
    && podman --version && skopeo --version \
    && /usr/libexec/tilo/finalize-rpmdb
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
