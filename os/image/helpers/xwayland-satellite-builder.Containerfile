# ---- xwayland-satellite -----------------------------------------------------
# The X11 bridge, built from pinned upstream source with the patch in
# os/external-assets/xwayland-satellite/ that keeps a paste in flight while the
# clipboard manager takes over an X11 selection, at an upstream commit that no
# release reaches yet: Fedora's 0.8.2 package lacks both. CI builders install the same build through the same script. Built in the
# pinned builder image like bootc, with the build headers from the frozen RPM
# snapshot; only the binary ships, and the rootfs checks it resolves
# against the base's libraries. Once a satellite release contains that commit
# and https://github.com/Supreeeme/xwayland-satellite/pull/527, install the
# snapshot's package instead.
FROM ${BOOTC_BUILD_IMAGE} AS xwayland-satellite-builder
# Buildah leaves the rpms mount out of the layer cache key; this ARG keys it on RPMS_REF.
ARG RPMS_REF
COPY os/external-assets/xwayland-satellite/0001-drain-pending-conversions-before-takeover.patch /patches/
COPY os/external-assets/github-assets.tsv /tilo-assets.tsv
COPY tools/ci/install-xwayland-satellite.sh /install-xwayland-satellite.sh
# Fetches the crates its Cargo.lock pins, so it runs in the build job's own network:
# podman hands a nested build container public resolvers (the job's /etc/resolv.conf names
# only its own), and the CI runners' egress serves those unreliably.
RUN --network=host --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,from=rpms,source=/repo,target=/repo \
    dnf install -y --setopt=reposdir=/repo xcb-util-cursor-devel \
    && dnf clean all \
    && bash /install-xwayland-satellite.sh /tilo-assets /out/usr /tilo-assets.tsv /patches
