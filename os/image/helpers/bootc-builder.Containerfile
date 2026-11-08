# ---- bootc ------------------------------------------------------------------
# The base's own bootc release, rebuilt with os/external-assets/bootc-runtime/:
# on a disk shared with another operating system, `bootc status`, `upgrade`
# and `switch` use the ESP the boot loader started from. Upstream takes the
# disk's first ESP, which on a laptop that came with Windows is Windows', and
# every one of them then fails reading loader entries that ESP does not have.
# And an upgrade needs room only for the layers it does not have yet (0003):
# upstream's composefs space check counted the whole image, so a laptop with
# most of the new image's layers already stored refused to upgrade.
# The frozen snapshot carries no ostree-devel, so this builds in the pinned
# builder image as os/Containerfile.bootc-helper does, from that image's own
# Fedora repositories, which os/dev/configure-fedora-mirrors.sh points at two
# current mirrors (BOOTC_BUILD_PACKAGES are no snapshot input). Nothing of it
# ships but the binary: the rootfs checks it runs against the base's own
# libraries and reports the base's bootc version.
FROM ${BOOTC_BUILD_IMAGE} AS bootc-builder
# v1.16.13, the base's bootc release.
ARG BOOTC_VERSION=1.16.13
ARG BOOTC_SOURCE=bootc-fa0d3f9.tar.gz
ARG BOOTC_SHA256=e75e0d10752aacfcac041d88f0f5099ba60248b046f1eb135ae886a4d58019ac
ARG BOOTC_BUILD_PACKAGES="ostree-devel patch git"
COPY os/external-assets/bootc-runtime/0001-select-booted-esp.patch \
    os/external-assets/bootc-runtime/0002-zlink-macros-from-tilo-vendor.patch \
    os/external-assets/bootc-runtime/0003-space-check-counts-missing-layers.patch /patches/
# zlink-macros with its generated enum variants in a fixed order (0002 patches
# bootc's build onto it): the release emits them in HashMap order, so no two
# bootc builds matched and the image was not reproducible.
COPY vendor/zlink-macros /tilo-vendor/zlink-macros
COPY os/dev/configure-fedora-mirrors.sh /usr/local/bin/configure-fedora-mirrors
# Fetches from the internet, so it runs in the build job's own network: podman hands a
# nested build container public resolvers (the job's /etc/resolv.conf names only its own),
# and the CI runners' egress serves those unreliably.
# What it fetches includes a dependency that lives in a git repository, and a connection
# that stops delivering leaves such a fetch waiting without end: libgit2 has no timeout of
# its own, so the build then holds its job until it is cancelled. Cargo asks the git
# command for that dependency, and the command aborts a transfer that stays below
# http.lowSpeedLimit for http.lowSpeedTime, which cargo retries.
RUN --network=host --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    configure-fedora-mirrors \
    && dnf install -y ${BOOTC_BUILD_PACKAGES} \
    && dnf clean all \
    && git config --global http.lowSpeedLimit 1000 \
    && git config --global http.lowSpeedTime 20 \
    && echo "${BOOTC_SHA256}  /tilo-assets/${BOOTC_SOURCE}" | sha256sum -c - \
    && mkdir "/bootc-${BOOTC_VERSION}" \
    && tar -xzf "/tilo-assets/${BOOTC_SOURCE}" --strip-components=1 -C "/bootc-${BOOTC_VERSION}" \
    && cd "/bootc-${BOOTC_VERSION}" \
    && for p in /patches/*.patch; do patch -p1 < "$p"; done \
    && export CARGO_NET_GIT_FETCH_WITH_CLI=true CARGO_NET_RETRY=6 \
    && cargo test --locked -p bootc-internal-blockdev \
    && CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=symbols \
       cargo build --locked --release -p bootc \
    && install -D -m 0755 target/release/bootc /out/usr/bin/bootc \
    && install -D -m 0644 -t /out/usr/share/licenses/tilo-bootc/ \
       LICENSE-APACHE LICENSE-MIT /patches/*.patch \
    && printf '%s\n' \
      'bootc - boot and upgrade via container images' \
      'License: Apache-2.0 OR MIT (see LICENSE-APACHE, LICENSE-MIT)' \
      "Source: https://github.com/bootc-dev/bootc/tree/fa0d3f9cb9a0ce3b4d1dc2607a0bf5e31b822f60 (v${BOOTC_VERSION})" \
      'Modified: /usr/bin/bootc is built from the source above with the patches here.' \
      > /out/usr/share/licenses/tilo-bootc/NOTICE \
    && rm -rf "/bootc-${BOOTC_VERSION}"
