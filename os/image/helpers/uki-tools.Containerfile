# A throwaway builder, so ukify and binutils do not ship in the OS image. It comes
# out of the same digest-pinned base and the same frozen RPM snapshot as
# everything else, so no stage of this build reaches the network. The UKI is
# self-contained: its EFI stub comes from this stage's systemd-boot and does not
# have to match the systemd in the rootfs.
#
# Every sealed image (this file, os/Containerfile.board,
# os/Containerfile.dev-installer-base) carries this stage and the `uki` stage
# below with the same instructions; tools/policy/check_uki_pipeline.py fails
# when one copy changes alone. Only the FROM line may differ.
FROM ${BASE_REF} AS uki-tools
# Buildah leaves the rpms mount out of the layer cache key; this ARG keys it on RPMS_REF.
ARG RPMS_REF
COPY os/dev/uki-tools.packages /usr/local/share/tilo/uki-tools.packages
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo \
    grep -v '^#' /usr/local/share/tilo/uki-tools.packages \
    | xargs dnf install -y --setopt=reposdir=/repo \
    && dnf clean all
COPY os/dev/uki.sh os/dev/uki-hardware-profile.py os/dev/hardware_profile.py os/dev/make-boot-splash.py /usr/local/bin/
