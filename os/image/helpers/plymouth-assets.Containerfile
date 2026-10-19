# ---- The Plymouth theme, rendered -------------------------------------------
# The splash's PNGs are rendered at build time by os/dev/make-plymouth-assets.py,
# so the source remains reviewable. It needs Pillow, which the frozen snapshot
# carries through systemd-ukify, and fc-match to find the catalog's Noto Sans,
# mounted where the rootfs carries it. Rendering here rather than in the rootfs
# keeps Pillow out of the image and a theme edit out of its package layer;
# rootfs-base copies /out/tilo.
FROM ${BASE_REF} AS plymouth-assets
# Buildah leaves the rpms mount out of the layer cache key; this ARG keys it on RPMS_REF.
ARG RPMS_REF
COPY os/usr/share/plymouth/themes/tilo/ /out/tilo/
COPY os/dev/make-plymouth-assets.py /usr/local/bin/make-plymouth-assets.py
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo \
    --mount=type=bind,from=os-fonts,source=/out/fonts,target=/usr/share/fonts/tilo,ro \
    --mount=type=tmpfs,target=/tmp \
    dnf install -y --setopt=reposdir=/repo systemd-ukify fontconfig \
    && XDG_CACHE_HOME=/tmp/fontconfig-cache python3 /usr/local/bin/make-plymouth-assets.py /out/tilo \
    && test -s /out/tilo/tilo-logo.png \
    && dnf clean all
