# ---- bootc ------------------------------------------------------------------
# The base's own bootc release, rebuilt with os/external-assets/bootc-runtime/:
# on a disk shared with another operating system, `bootc status`, `upgrade`
# and `switch` use the ESP the boot loader started from. Upstream takes the
# disk's first ESP, which on a laptop that came with Windows is Windows', and
# every one of them then fails reading loader entries that ESP does not have.
# And an upgrade needs room only for the layers it does not have yet (0003):
# upstream's composefs space check counted the whole image, so a laptop with
# most of the new image's layers already stored refused to upgrade.
# And the installer's copy of the booted image writes its layers uncompressed
# (0005): they were gzipped on one thread only to be unpacked again.
# And every finalize of a staged deployment merges its /etc again (0006), so
# the one at shutdown carries over what /etc became after the finalize Tilo
# makes right after staging.
# It builds in the OS builder (the base's own libraries, Fedora's Rust), with
# the ostree, zstd and OpenSSL headers it links against from the frozen RPM snapshot: the CI
# builder image is an older Fedora, whose installed libraries the snapshot's
# headers cannot be installed beside. So every input is pinned (the crates by
# bootc's Cargo.lock) and the build is served from t's cache while they are as
# they were. Nothing of it ships but the binary: the rootfs checks it runs
# against the base's own libraries and reports the base's bootc version.
FROM builder-base AS bootc-builder
# v1.16.13, the base's bootc release.
ARG BOOTC_VERSION=1.16.13
ARG BOOTC_SOURCE=bootc-fa0d3f9.tar.gz
ARG BOOTC_SHA256=e75e0d10752aacfcac041d88f0f5099ba60248b046f1eb135ae886a4d58019ac
# composefs-rs v0.9.2, the git dependency bootc's Cargo.lock pins, from its source
# archive (0004 points bootc's build at it): GitHub's git transfers to the image
# build's network kept resetting, and cargo's retries ended the build with them.
ARG COMPOSEFS_SOURCE=composefs-rs-f720d6c.tar.gz
ARG COMPOSEFS_SHA256=ab52c9b237501aab5a02e36cab313a37efb66374d9a4cd0ac4eed56df15413d3
# Buildah leaves the rpms mount out of the layer cache key; this ARG keys it on RPMS_REF.
ARG RPMS_REF
COPY os/external-assets/bootc-runtime/0001-select-booted-esp.patch \
    os/external-assets/bootc-runtime/0002-zlink-macros-from-tilo-vendor.patch \
    os/external-assets/bootc-runtime/0003-space-check-counts-missing-layers.patch \
    os/external-assets/bootc-runtime/0004-composefs-rs-from-its-source-archive.patch \
    os/external-assets/bootc-runtime/0005-export-uncompressed-layers.patch \
    os/external-assets/bootc-runtime/0006-merge-etc-at-every-finalize.patch /patches/
# zlink-macros with its generated enum variants in a fixed order (0002 patches
# bootc's build onto it): the release emits them in HashMap order, so no two
# bootc builds matched and the image was not reproducible.
COPY vendor/zlink-macros /tilo-vendor/zlink-macros
# Fetches the crates bootc's Cargo.lock pins, so it runs in the build job's own network:
# podman hands a nested build container public resolvers (the job's /etc/resolv.conf names
# only its own), and the CI runners' egress serves those unreliably.
RUN --network=host --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,from=rpms,source=/repo,target=/repo \
    dnf install -y --setopt=reposdir=/repo ostree-devel libzstd-devel openssl-devel patch \
    && dnf clean all \
    && echo "${BOOTC_SHA256}  /tilo-assets/${BOOTC_SOURCE}" | sha256sum -c - \
    && echo "${COMPOSEFS_SHA256}  /tilo-assets/${COMPOSEFS_SOURCE}" | sha256sum -c - \
    && mkdir /composefs-rs \
    && tar -xzf "/tilo-assets/${COMPOSEFS_SOURCE}" --strip-components=1 -C /composefs-rs \
    && mkdir "/bootc-${BOOTC_VERSION}" \
    && tar -xzf "/tilo-assets/${BOOTC_SOURCE}" --strip-components=1 -C "/bootc-${BOOTC_VERSION}" \
    && cd "/bootc-${BOOTC_VERSION}" \
    && for p in /patches/*.patch; do patch -p1 < "$p"; done \
    && export CARGO_NET_RETRY=6 \
    && cargo test --locked -p bootc-internal-blockdev -p etc-merge \
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
    && rm -rf "/bootc-${BOOTC_VERSION}" /composefs-rs
