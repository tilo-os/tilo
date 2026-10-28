# ---- Joy-Con pairing daemon -------------------------------------------------
# joycond (GPL-3.0-only) folds a left and a right Joy-Con into one uinput
# controller, or releases a single half to be held sideways. Fedora carries it
# only in COPR, which the frozen snapshot's signature policy cannot admit
# (os/dev/verify-rpm-snapshot-signatures.sh), so build the pinned upstream
# commit from snapshot build dependencies. Its udev rules are replaced by
# os/usr/lib/udev/rules.d/72-joycond.rules, which scopes it to Joy-Cons, and
# its modules-load snippet moves from /etc to the immutable /usr/lib.
# tilo-rpm-snapshot-package: libevdev-devel
FROM ${BASE_REF} AS joycond-builder
# Buildah leaves the rpms mount out of the layer cache key; this ARG keys it on RPMS_REF.
ARG RPMS_REF
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo \
    dnf install -y --setopt=reposdir=/repo \
      cmake gcc-c++ make pkgconf-pkg-config libevdev-devel systemd-devel \
    && dnf clean all
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    mkdir -p /src \
    && tar -xzf /tilo-assets/joycond-0df025a.tar.gz --strip-components=1 -C /src \
    && cmake -S /src -B /build -DCMAKE_BUILD_TYPE=Release \
    && cmake --build /build --parallel \
    && DESTDIR=/out cmake --install /build \
    && install -D -m 0644 /out/etc/modules-load.d/joycond.conf \
      /out/usr/lib/modules-load.d/joycond.conf \
    && test -x /out/usr/bin/joycond \
    && test -f /out/usr/lib/systemd/system/joycond.service
