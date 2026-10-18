# ---- The derived state: what reads every layer below ----------------------
# The initramfs (the kernel's modules, the rollback and unlock helpers, the
# public PCR key), the font and linker caches, the hardware database, the
# generation, and the checks of the finished rootfs. Each is a function of the
# layers below it and SOURCE_DATE_EPOCH, so the same layers give the same bytes
# here; nothing in this piece is copied in from outside. It reruns whenever a
# layer below changes, and is small: whatever would make it large (a chmod of a
# binary, a rewritten package file) belongs in the layer that owns the file.
FROM binaries AS rootfs
# The build-only dnf/rpm wrappers need it for the removals below, and the
# initramfs and font cache are dated by it.
ARG SOURCE_DATE_EPOCH=0
ARG TILO_HARDWARE_PROFILE=generic
ARG TILO_TARGET_IMGREF=

# The board's hardware profile (its firmware and hashed device tree) and the
# initramfs, now that the rollback and unlock helpers and the public trust it
# carries are in place. dracut runs with --reproducible, so its archive is dated
# by SOURCE_DATE_EPOCH, not the build: the same layers give the same initramfs.
RUN --mount=type=bind,source=os/hardware/firmware/${TILO_HARDWARE_PROFILE},target=/run/tilo-hardware-firmware \
    --mount=type=bind,source=os/dev/prepare-hardware-profile.py,target=/run/tilo-prepare-hardware-profile.py \
    --mount=type=bind,source=os/scripts/build-initramfs.sh,target=/run/tilo-build-initramfs.sh \
    --mount=type=bind,source=os/scripts/verify-board-initramfs.py,target=/run/verify-board-initramfs.py \
    --mount=type=bind,source=os/dev/hardware_profile.py,target=/run/hardware_profile.py \
    --mount=type=bind,source=os/hardware/profiles,target=/run/tilo-hardware-profiles \
    set -eux; \
    set -- /usr/lib/modules/*/vmlinuz; \
    [ "$#" -eq 1 ] && [ -f "$1" ] || { echo "expected exactly one kernel in /usr/lib/modules, found $#" >&2; exit 1; }; \
    kver="$(basename "$(dirname "$1")")"; \
    # -B: the script imports hardware_profile from /run, and its bytecode
    # (which records the checkout's mtime) would otherwise stay in the layer.
    python3 -B /run/tilo-prepare-hardware-profile.py \
        --profile "$TILO_HARDWARE_PROFILE" --root / --kernel-version "$kver" \
        --profiles-dir /run/tilo-hardware-profiles \
        --firmware-dir "/run/tilo-hardware-firmware" \
        --target-update-ref "$TILO_TARGET_IMGREF"; \
    bash /run/tilo-build-initramfs.sh "$kver"

# Boot-chain invariants, asserted at BUILD time so the image cannot ship in the
# state it once did: offering `tilo-install --encrypt` while carrying no
# initramfs, which leaves the kernel able to see the LUKS device and unable to
# open it. Also bundled so an operator can re-run it on a live machine.
COPY os/tests/boot_chain_invariants.sh /usr/libexec/tilo/boot-chain-test.sh
RUN chmod 0755 /usr/libexec/tilo/boot-chain-test.sh \
    && OS_ROOT=/ bash /usr/libexec/tilo/boot-chain-test.sh

# Enrolment invariants, likewise at BUILD time. A build container has no
# /dev/tpm*, which is exactly the path this needs to exercise: the machine with
# no TPM, whose owner has nothing but the passphrase they were asked for. The
# image once shipped an `enrol` that added no passphrase key slot on that path,
# so the volume it produced could not be opened by the secret it had just asked
# for and printed no usable recovery key either. Runs against a throwaway sparse
# file, needs no device-mapper, and takes about ten seconds.
COPY os/tests/luks_enrolment.sh /usr/libexec/tilo/luks-enrolment-test.sh
RUN chmod 0755 /usr/libexec/tilo/luks-enrolment-test.sh \
    && bash /usr/libexec/tilo/luks-enrolment-test.sh

# ---- The generation --------------------------------------------------------
# Last before the final check, and not beside the rollback machinery it belongs
# to, because it is the one input that changes with every published image: an
# ARG keys every step after it, so declaring it earlier rebuilt everything from
# here back to the package transactions whenever the series moved on.
#
# The generation number this image carries. It MUST be a dense monotonic integer
# (+1 per published image), because the retention window is arithmetic on it —
# "the last three generations" is meaningless if the numbering is a timestamp, and
# moving the floor costs one TPM NV write per step (tilo-rollback refuses an
# advance of more than MAX_INCREMENTS_PER_ADVANCE at all). os/dev/build-image.sh
# resolves it from the published series; the CI job that publishes an image
# derives it the same way and tags the result `gen<N>`.
#
# It reaches the running system by two routes, for two different jobs:
#
#   /usr/lib/bootc/kargs.d/02-generation.toml
#       `bootc container ukify` reads this directory and puts what it finds on the
#       UKI's sealed command line, which systemd-stub measures into PCR 11. That
#       is the authoritative copy — inside the signature, measured, and therefore
#       the one an unseal can be bound to. A number the bootloader assembled from
#       an editable file would look identical and be worth nothing.
#   /usr/lib/tilo/generation
#       A plain file holding the same number, for the one question the command
#       line cannot answer: what generation is the deployment that has been
#       *staged* but never booted? Its keyslot has to be sealed before the reboot
#       that first runs it. Not trusted for anything — at that moment the machine
#       is already unlocked.
#
# Both are written from the same build argument in the same step, so they cannot
# disagree; rollback-boot-check.sh asserts on a booted machine that they don't.
#
# There is deliberately no default. A default is what a constant looks like once
# a caller forgets the argument, and an image that silently ships generation 0 is
# one whose rollback floor can never move — the failure would appear months later
# as a machine that will not unseal.
ARG TILO_GENERATION
RUN case "${TILO_GENERATION}" in \
      ''|*[!0-9]*) \
        echo "TILO_GENERATION must be a decimal integer; got '${TILO_GENERATION}'." >&2; \
        echo "It is the generation this image carries: +1 per published image, sealed" >&2; \
        echo "onto the UKI command line. os/dev/build-image.sh resolves it from the" >&2; \
        echo "published series; pass TILO_GENERATION=<N> to build one by hand." >&2; \
        exit 1 ;; \
    esac \
 && install -d -m 0755 /usr/lib/tilo \
 && printf '%s\n' "${TILO_GENERATION}" > /usr/lib/tilo/generation \
 && printf '# Written by the image build from the TILO_GENERATION build argument.\nkargs = ["tilo.generation=%s"]\n' \
      "${TILO_GENERATION}" > /usr/lib/bootc/kargs.d/02-generation.toml \
 && echo "generation ${TILO_GENERATION}: sealed via kargs.d, readable at /usr/lib/tilo/generation"

# The shell and the browser engine must resolve every library they load from
# this rootfs: a gap is a loader error at the first launch, on a device.
ARG TILO_WITH_BROWSER
ARG CHROMIUM_RUNTIME_VERSION
RUN --mount=type=bind,source=os/tests/collaborative-files-image-contract.sh,target=/run/tilo-collaborative-files-image-contract.sh,ro \
    for f in /usr/bin/tilo-relay /usr/bin/tilo-relay-admin /usr/bin/tilo-enroll \
        /usr/libexec/tilo-home-peer-control /usr/libexec/tilo-relay-firstboot \
        /usr/libexec/tilo-windows-vm-control /usr/libexec/tilo-windows-rail \
        /usr/bin/shell /usr/bin/fs /usr/bin/tilo-greeter \
        /usr/bin/tilo-installer /usr/bin/tilo-launcher /usr/bin/tilo-runtime-bundle \
        /usr/bin/tilo-install /usr/bin/tilo-app-package /usr/libexec/tilo-cli-shim \
        /usr/libexec/tilo-unlock-helper /usr/libexec/tilo-ush \
        /usr/libexec/tilo-projector /usr/libexec/tilo-doubletake \
        /usr/libexec/tilo-doubletake-test-receiver \
        /usr/libexec/tilo-account-credential /usr/libexec/tilo-prepared-setup; do \
      test -e "$f" || { echo "$f is not in the image" >&2; exit 1; }; \
      [ "$(stat -c %a "$f")" = 755 ] || chmod 0755 "$f"; \
    done \
    && missing="$(ldd /usr/bin/shell /usr/bin/xwayland-satellite | awk '/not found/ { print $1 }')" \
    && { if [ -n "$missing" ]; then \
         printf 'shell or xwayland-satellite has unresolved runtime libraries:\n%s\n' "$missing" >&2; \
         exit 1; \
       fi; } \
    && engine="/usr/lib/tilo-runtime/${CHROMIUM_RUNTIME_VERSION}/lib/chromium" \
    && { if [ "${TILO_WITH_BROWSER}" != 0 ]; then \
         test -x "$engine/tilo-chromium-host" \
         && test -s "$engine/libcef.so" \
         && /usr/bin/tilo-runtime-bundle verify \
              "/usr/lib/tilo-runtime/${CHROMIUM_RUNTIME_VERSION}" \
              --require-publisher "$(cat /usr/share/tilo/browser-runtime-publisher.hex)"; \
       fi; } \
    && { [ ! -d "$engine" ] || { \
         missing="$(LD_LIBRARY_PATH="$engine" ldd "$engine/tilo-chromium-host" "$engine/libcef.so" \
                    | awk '/not found/ { print $1 }' | sort -u)" \
         && if [ -n "$missing" ]; then \
              printf 'the browser engine has unresolved runtime libraries:\n%s\n' "$missing" >&2; \
              exit 1; \
            fi; }; } \
    && { /usr/libexec/tilo-doubletake -h >/dev/null \
         || { echo 'doubletake help smoke test failed' >&2; exit 1; }; } \
    && mkdir -p /etc/tilo \
    && cat /usr/share/tilo/runtime-bundle-publisher.hex \
        > /etc/tilo/runtime-bundle-trusted-publishers.txt \
    && cat /usr/share/tilo/browser-runtime-publisher.hex \
        >> /etc/tilo/runtime-bundle-trusted-publishers.txt \
    && cat /usr/share/tilo/registry-runtime-publisher.hex \
        >> /etc/tilo/runtime-bundle-trusted-publishers.txt \
    && OS_ROOT=/ bash /run/tilo-collaborative-files-image-contract.sh image \
    # Observe the installed helper and exact signed SDK through its own loader.
    # Optional service-session artifacts absent means no service_sessions proof.
    && /usr/libexec/tilo-prepared-setup --verify-account-consumer-capability \
    && /usr/libexec/tilo-prepared-setup --verify-account-consumer-capability --check >/dev/null \
    && /usr/libexec/tilo-prepared-setup --verify-home-consumer-capability \
    && /usr/libexec/tilo-prepared-setup --verify-home-consumer-capability --check >/dev/null

# Validate.
#
# This stays at the end of the rootfs stage, ahead of the kernel split rather than
# moved before it defensively.
# The worry was that lint would reject a split-kernel rootfs — `bootc container
# split-kernel-and-rootfs` moves vmlinuz and initramfs.img out of
# /usr/lib/modules/$kver and into a UKI under /boot — and would have to run
# before the split. It does not: checked against a built split-kernel UKI image
# under bootc 1.16.4, lint exits 0. The `kernel` check counts
# /usr/lib/modules/$kver *directories*, and the split leaves that directory in
# place, so one kernel is still one kernel; `nonempty-boot` is a warning, not a
# fatal, so a UKI in /boot does not fail it either.
# Fedora's OpenSSL libraries carry optional legacy-provider and ENGINE DSOs for
# compatibility with old applications. The GStreamer bad-free bundle also
# carries OpenSSL-backed DTLS, curl, and Icecast/Shoutcast plugins; its WebRTC,
# ICE and SRTP companions are unusable here and likewise serve no Tilo path.
# libevent's OpenSSL adapter is a separate optional DSO; retain the event-loop
# libraries for distro consumers but remove that unused TLS integration.
# Rust-native code owns network security, while foundational distro consumers
# use the built-in provider. The Shoutcast library has no remaining consumer after its plugin is
# removed, so delete its payload and package metadata too. The aggregate
# GStreamer bundle retains a dependency on libshout for the deleted plugin;
# `--nodeps` is therefore intentional and bounded by the absence audit below.
# wine-common's helper scripts need a Perl interpreter, and Fedora's Perl
# core pulls in HTTP::Tiny's HTTPS backend (IO::Socket::SSL over the
# Net::SSLeay OpenSSL binding). No Tilo path runs Perl networking, so remove
# that backend the same way; the forbidden-package gate below enforces it.
# Delete these loadable surfaces before the composefs root is sealed.
# Then drop build-time /run, /tmp and /var state (issue #71) so bootc lint
# does not retain nonempty-run-tmp, var-log or unpackaged var-tmpfiles
# warnings. greetd's portal mask is declared in tmpfiles.d/tilo.conf and is
# recreated on first boot rather than baked into the image.
# Replace the runtime symlink with a build-only regular file in a separate
# layer. Its current RUN may still mount DNS under /run; the next one will not.
RUN rm -f /etc/resolv.conf && printf '# build-only resolver placeholder\n' > /etc/resolv.conf
RUN --mount=type=bind,source=os/scripts/clean-bootc-transient-state.py,target=/usr/local/libexec/tilo-clean-bootc-transient-state.py,ro \
    --mount=type=bind,source=os/usr/lib/systemd/system/ldconfig.service.d/10-tilo-image-cache.conf,target=/usr/local/libexec/tilo-ldconfig-image-cache.conf,ro \
    --mount=type=bind,source=os/scripts/verify-boot-ordering.sh,target=/usr/local/libexec/tilo-verify-boot-ordering.sh,ro \
    --mount=type=bind,source=os/scripts/verify-rpm-files.sh,target=/usr/local/libexec/tilo-verify-rpm-files.sh,ro \
    --mount=type=bind,source=os/scripts/pin-directory-times.py,target=/usr/local/libexec/tilo-pin-directory-times.py,ro \
    --mount=type=bind,source=os/scripts/seed-env-runtime.py,target=/usr/local/libexec/tilo-seed-env-runtime.py,ro \
    --mount=type=bind,from=native-builder-payload,source=/usr/share/tilo-apps,target=/usr/local/libexec/tilo-apps-built,ro \
    find /usr/lib /usr/lib64 \
      \( -path '*/ossl-modules/legacy.so' -o -path '*/engines-3/*.so' \
         -o -name 'libshout.so*' \
         -o -name 'libevent_openssl.so*' \
         -o -path '*/gstreamer-1.0/libgstdtls.so' \
         -o -path '*/gstreamer-1.0/libgstcurl.so' \
         -o -path '*/gstreamer-1.0/libgstshout2.so' \
         -o -path '*/gstreamer-1.0/libgstwebrtc.so' \
         -o -path '*/gstreamer-1.0/libgstnice.so' \
         -o -path '*/gstreamer-1.0/libgstsrtp.so' \) \
      \( -type f -o -type l \) -delete \
    && rpm -e --nodeps libshout \
    && rpm -qa --qf '%{NAME}\n' perl-IO-Socket-SSL perl-Net-SSLeay qatlib-service qatlib \
       | xargs -r rpm -e --nodeps \
    && ! find /usr/lib /usr/lib64 \
      \( -path '*/ossl-modules/legacy.so' -o -path '*/engines-3/*.so' \
         -o -name 'libshout.so*' \
         -o -name 'libevent_openssl.so*' \
         -o -path '*/gstreamer-1.0/libgstdtls.so' \
         -o -path '*/gstreamer-1.0/libgstcurl.so' \
         -o -path '*/gstreamer-1.0/libgstshout2.so' \
         -o -path '*/gstreamer-1.0/libgstwebrtc.so' \
         -o -path '*/gstreamer-1.0/libgstnice.so' \
         -o -path '*/gstreamer-1.0/libgstsrtp.so' \) \
      \( -type f -o -type l \) -print -quit | grep -q . \
    && bash /usr/local/bin/verify-runtime-crypto-packages.sh \
    && bash /usr/local/bin/verify-no-ui-toolkits.sh \
    && bash /usr/local/bin/verify-no-cisco-openh264.sh \
    # The rpmdb finaliser ran after every package change; none comes now.
    && rm -f /usr/libexec/tilo/finalize-rpmdb \
    && bash /usr/local/bin/verify-no-python-runtime.sh \
    && rm -f /usr/local/bin/verify-runtime-crypto-packages.sh /usr/local/bin/verify-no-ui-toolkits.sh \
             /usr/local/bin/verify-no-cisco-openh264.sh /usr/local/bin/verify-no-python-runtime.sh \
             /usr/local/bin/dnf /usr/local/bin/rpm /usr/libexec/tilo/build-clock.so \
    # The system font cache, written once here: fontconfig records each font
    # directory's time, so those are pinned to the epoch first, the time the
    # image carries, and the cache is the same in every build and valid on
    # the booted system.
    && for fonts in /usr/share/fonts /usr/share/X11/fonts /usr/local/share/fonts; do \
         if [ -d "$fonts" ]; then python3 -B /usr/local/libexec/tilo-pin-directory-times.py "$fonts" "$SOURCE_DATE_EPOCH"; fi; \
       done \
    && HOME=/root fc-cache -s -f \
    # The generic families resolve from the catalog (57-tilo-fonts.conf), which
    # arrived with the runtime bundle.
    && for generic in 'Noto Sans CJK SC' serif monospace emoji; do \
           case "$(fc-match -f '%{file}' "$generic")" in \
               /usr/share/fonts/tilo/*) ;; \
               *) echo "fontconfig does not resolve $generic from the catalog" >&2; exit 1 ;; \
           esac; \
       done \
    # The linker cache is current as built; ldconfig.service rebuilds it only
    # if it goes missing (its drop-in). The hardware database lives in /usr
    # with none in /etc, so systemd-hwdb-update.service has nothing to do on
    # the first boot of a deployment either.
    && install -D -m 0644 /usr/local/libexec/tilo-ldconfig-image-cache.conf \
         /usr/lib/systemd/system/ldconfig.service.d/10-tilo-image-cache.conf \
    && ldconfig \
    && systemd-hwdb update --usr \
    && rm -f /etc/udev/hwdb.bin \
    && test -s /usr/lib/udev/hwdb.bin \
    && { [ ! -d /etc/udev/hwdb.d ] || [ -z "$(ls -A /etc/udev/hwdb.d)" ]; } \
    && /usr/lib/systemd/systemd-update-done --root=/ \
    && test -s /etc/.updated \
    # The store's env-runtime takes some of its files from the host
    # (app-store): each must still be what it stood in for now that nothing
    # changes /usr any more; one that is not ships in the store again.
    && python3 /usr/local/libexec/tilo-seed-env-runtime.py heal \
         /usr/share/tilo-apps /usr/local/libexec/tilo-apps-built / \
    && python3 /usr/local/libexec/tilo-clean-bootc-transient-state.py \
    && bash /usr/local/libexec/tilo-verify-boot-ordering.sh \
    && sh /usr/local/libexec/tilo-verify-rpm-files.sh \
    && test "$(bootc --version)" = "bootc $(rpm -q --qf '%{VERSION}' bootc)" \
    && bootc container lint

# COPY is a separate build operation, with no DNS mount in /run. Copy the
# containing directory so builders preserve its only entry, the symlink,
# instead of dereferencing a single-file source with an absent /run target.
# Restore the NetworkManager resolver before split/ukify seal the rootfs.
COPY --from=runtime-resolver-link /out/ /etc/

