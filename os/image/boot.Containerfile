# ---------------------------------------------------------------------------
# The sealed layout: split the kernel out, build the UKI over what remains
# ---------------------------------------------------------------------------
# Following bootc's documented sealed-image pattern:
#
#   kernel  the kernel and initramfs moved out of the rootfs into /kernel/<kver>,
#           for the UKI to embed. A throwaway stage: nothing ships from it.
#   split   the same move in the shipped rootfs — bootc refuses to seal an image
#           that still carries them in /usr/lib/modules/<kver>, and they belong
#           in the UKI now.
#   uki     `bootc container ukify` over the split rootfs, so the composefs digest
#           it seals is the digest of what will actually be mounted.
#   final   the split rootfs plus the UKI under /boot, which is the one tree
#           excluded from that digest.
#
# `split` keeps /kernel/<kver>/vmlinuz, from which the signed development board
# (os/Containerfile.board) restores the kernel exactly as built, and drops the
# initramfs: the UKI's .initrd section is the same bytes, and the dev image
# restores it from there (os/scripts/unseal-rootfs.py). Kept, it was a third
# 102 MB copy in every pull, beside the UKI and the original the split whites
# out.
#
# Nothing may be added to the rootfs after the split: the digest was computed over
# it, and a later layer would invalidate the seal. That is why the sealed command
# line and ukify's measurement report are pulled off the `uki` stage by
# os/dev/build-image.sh instead of being copied into the image.

FROM rootfs AS kernel
RUN mkdir /kernel && bootc container split-kernel-and-rootfs --rootfs / --output /kernel

FROM rootfs AS split
RUN test "$(readlink /etc/resolv.conf)" = ../run/NetworkManager/resolv.conf \
    && mkdir /kernel && bootc container split-kernel-and-rootfs --rootfs / --output /kernel \
    && rm /kernel/*/initramfs.img

# BuildKit keys a RUN by a content hash of every bind mount, read-only or not,
# so mounting the whole split rootfs into the uki stage hashed every file of it,
# about two minutes of every build. With a composefs digest given, uki.sh reads
# only these files of the rootfs; this stage hands it just those.
FROM split AS uki-input
RUN mkdir /uki-input \
    && cd / \
    && for path in usr/lib/os-release usr/lib/bootc/kargs.d \
                   usr/lib/tilo/hardware-profile.json usr/lib/tilo/hardware-profile-definition.json \
                   usr/lib/modules/*/dtb; do \
         if [ -e "$path" ] || [ -L "$path" ]; then cp -a --parents "$path" /uki-input/; fi; \
       done

FROM uki-tools AS uki
# The first pass uses a deterministic all-zero placeholder. `bootc install`
# computes the authoritative digest and os/dev/build-image.sh rewrites only the
# UKI under /boot before retrying. This avoids constructing a rootfs-sized
# composefs scratch repository merely to obtain a provisional value.
ARG TILO_CFS_DIGEST=
ENV CFS_DIGEST=${TILO_CFS_DIGEST}
# The negative control: 0 builds a UKI that measures perfectly and carries no
# generation, which is what proves rollback-boot-check.sh fails on the generation
# assertions rather than on "nothing measured". Always 1 for a built image.
ARG TILO_SEAL_GENERATION=1
ENV TILO_SEAL_GENERATION=${TILO_SEAL_GENERATION}
# /tmp and /run are tmpfs because ukify needs ordinary build scratch but none of
# it belongs in the image layer. The authoritative composefs digest comes from
# the install gate, so this stage no longer needs a rootfs-sized /var/tmp cache.
# /target holds only uki-input's files, so bootc's own digest of it would be
# wrong: the digest must be given.
RUN --mount=type=tmpfs,target=/tmp \
    --mount=type=tmpfs,target=/run \
    --mount=type=bind,from=uki-input,source=/uki-input,target=/target,rw \
    --mount=type=bind,from=kernel,source=/kernel,target=/kernel,rw \
    { [ -n "$CFS_DIGEST" ] || { echo 'TILO_CFS_DIGEST is required (os/dev/build-image.sh passes a placeholder)' >&2; exit 1; }; } \
    && bash /usr/local/bin/uki.sh

# NOTHING MAY BE LAYERED ON THIS IMAGE. A derived image that adds a single file to
# the rootfs has a different composefs digest from the one its inherited UKI seals,
# and `bootc install` refuses it — correctly, since the seal no longer describes
# what would be mounted. Rebuilding the UKI in the derived image is not a way out
# either: the kernel and initramfs are no longer in the rootfs to build one from,
# they are sections of the UKI it inherited.
#
# So `os/Containerfile.test` and `os/Containerfile.installer`, which both do
# `FROM localhost/tilo:latest`, have to be built from the UNSEALED rootfs and sealed
# themselves — `podman build --target rootfs` gives them that. Until they are, the
# disk step for those two images fails at the digest check with the message
# os/dev/disk-image.sh prints, rather than producing something that boots wrong.
FROM split AS final
COPY --from=uki /out/*.efi /boot/EFI/Linux/
# Which Rust LTO the binaries in this image were linked with (TILO_RUST_LTO in
# the builder stage): `thin` for a published image, `off` for integration and
# local builds. os/dev/build-image.sh refuses to publish an image not labelled
# thin.
ARG TILO_RUST_LTO=thin
LABEL org.tilo.rust-lto="${TILO_RUST_LTO}"
