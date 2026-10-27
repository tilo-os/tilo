# ---- Minimal Windows paravirtual driver media -----------------------------
# Fedora's pinned virtio-win RPM expands to almost 2 GiB because it contains
# the same driver tree in several layouts. Tilo only consumes the bootable ISO
# and the signed guest-tools installer. Extract and authenticate those exact
# payloads in a throwaway stage instead of installing all duplicated files in
# the immutable OS. Keep the RPM in the frozen snapshot so this remains an
# offline, digest-pinned build input.
# tilo-rpm-snapshot-package: virtio-win
FROM ${BASE_REF} AS virtio-win-media
# Buildah leaves the rpms mount out of the layer cache key; this ARG keys it on RPMS_REF.
ARG RPMS_REF
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo,ro \
    /bin/bash -euo pipefail -c '\
      rpm_path=$(find /repo -maxdepth 1 -type f -name "virtio-win-*.rpm" -print -quit); \
      test -n "$rpm_path"; \
      mkdir -p /tmp/virtio /out/installer; \
      cd /tmp/virtio; \
      rpm2cpio "$rpm_path" \
        | cpio -idm --quiet \
            ./usr/share/virtio-win/virtio-win-0.1.302.iso \
            ./usr/share/virtio-win/installer/virtio-win-guest-tools.exe; \
      printf "%s  %s\n%s  %s\n" \
        303f7ae40dad495d6ae474fdc571df58958a4dbc5c37a522d80f9a203867949d \
        usr/share/virtio-win/virtio-win-0.1.302.iso \
        d8ae9ea1e943384ac195e012cd36f82c3161620e5fc9529be4e42e6f420e222d \
        usr/share/virtio-win/installer/virtio-win-guest-tools.exe \
        | sha256sum --check --strict; \
      mv usr/share/virtio-win/virtio-win-0.1.302.iso /out/virtio-win.iso; \
      mv usr/share/virtio-win/installer/virtio-win-guest-tools.exe \
        /out/installer/virtio-win-guest-tools.exe; \
      chmod 0644 /out/virtio-win.iso /out/installer/virtio-win-guest-tools.exe'
