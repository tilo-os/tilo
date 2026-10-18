# ---- xwayland-satellite -----------------------------------------------------
# The X11 bridge, built from pinned upstream source with the patch in
# os/external-assets/xwayland-satellite/ that keeps a paste in flight while the
# clipboard manager takes over an X11 selection. Fedora's 0.8.2 package drops
# it. CI builders install the same build through the same script. Built in the
# pinned builder image like bootc, with that image's own Fedora repositories for
# the build headers; only the binary ships, and the rootfs checks it resolves
# against the base's libraries. Once a satellite
# release contains https://github.com/Supreeeme/xwayland-satellite/pull/527,
# install the snapshot's package instead.
FROM ${BOOTC_BUILD_IMAGE} AS xwayland-satellite-builder
ARG XWAYLAND_SATELLITE_BUILD_PACKAGES="xcb-util-cursor-devel"
COPY os/external-assets/xwayland-satellite/0001-drain-pending-conversions-before-takeover.patch /patches/
COPY os/external-assets/github-assets.tsv /tilo-assets.tsv
COPY tools/ci/install-xwayland-satellite.sh /install-xwayland-satellite.sh
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    dnf install -y ${XWAYLAND_SATELLITE_BUILD_PACKAGES} \
    && dnf clean all \
    && bash /install-xwayland-satellite.sh /tilo-assets /out/usr /tilo-assets.tsv /patches
