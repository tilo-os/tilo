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
# A greeter linked without LTO (local builds' default, os/dev/build-image.sh) is over a
# third larger than a release's, so its initramfs budget is reported there, as each app's
# size budget is, and enforced only where the binaries are linked as releases are.
ARG TILO_RUST_LTO=thin

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
    TILO_GREETER_BUDGET="$([ "$TILO_RUST_LTO" = off ] && echo report || echo enforce)" \
        bash /run/tilo-build-initramfs.sh "$kver"

# Boot-chain invariants (the image once offered `tilo-install --encrypt` while
# carrying no initramfs, which leaves the kernel able to see the LUKS device and
# unable to open it) and enrolment invariants (an `enrol` once added no
# passphrase key slot on a machine with no TPM). Bundled so an operator can rerun
# them on a live machine; os-image-contracts runs both against the composed
# image, beside the build rather than inside it.
COPY --chmod=0755 os/tests/boot_chain_invariants.sh /usr/libexec/tilo/boot-chain-test.sh
COPY --chmod=0755 os/tests/luks_enrolment.sh /usr/libexec/tilo/luks-enrolment-test.sh

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

# Of the desktop's programs only those the consumer receipts certify run here: t
# lays the others over the finished image (os/image/binaries.Containerfile), and
# os-image-contracts checks them in the composed image (os/tests/image-programs-
# contract.sh). This checks that no package claims one of tilo's nouns as its
# command, the browser engine resolves every library it loads from this rootfs,
# and the AirPlay sender starts. Then the prepared-setup helper publishes the
# account and Home consumer receipts (/usr/share/tilo), which the installer's
# media validation and a device's first boot hold the image's consumers to.
ARG CHROMIUM_RUNTIME_VERSION
RUN for name in device action package agent system tilo-app tilo-action tilo-device tilo-package tilo-agent; do \
      if rpm --quiet -qf "/usr/bin/$name"; then \
        echo "/usr/bin/$name is tilo's noun and also $(rpm -qf "/usr/bin/$name")'s command" >&2; exit 1; \
      fi; \
    done \
    && engine="/usr/lib/tilo-runtime/${CHROMIUM_RUNTIME_VERSION}/lib/chromium" \
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
    && /usr/libexec/tilo-prepared-setup --verify-account-consumer-capability \
    && /usr/libexec/tilo-prepared-setup --verify-account-consumer-capability --check >/dev/null \
    && /usr/libexec/tilo-prepared-setup --verify-home-consumer-capability \
    && /usr/libexec/tilo-prepared-setup --verify-home-consumer-capability --check >/dev/null

# Finish the rootfs.
#
# The package payloads no Tilo path loads were removed at the end of the system
# layer (os/image/system.Containerfile), the last that changes packages; that none
# is back, in a package file or in anything copied in since, is checked here.
# What only reads the finished image runs in os-image-contracts against the
# composed image instead of inside this build: that no Python program of Tilo's
# ships (os/scripts/verify-no-python-runtime.sh), that every file of a package
# this build installed is what the rpmdb records (os/scripts/verify-rpm-files.sh),
# and `bootc container lint`. Lint passes on the split-kernel image: its `kernel`
# check counts /usr/lib/modules/$kver directories, which the split leaves in place,
# and `nonempty-boot` is a warning, not a fatal, so the UKI in /boot does not fail it.
# Then drop build-time /run, /tmp and /var state so bootc lint
# does not retain nonempty-run-tmp, var-log or unpackaged var-tmpfiles
# warnings. greetd's portal mask is declared in tmpfiles.d/tilo.conf and is
# recreated on first boot rather than baked into the image.
# Replace the runtime symlink with a build-only regular file in a separate
# layer. Its current RUN may still mount DNS under /run; the next one will not.
RUN rm -f /etc/resolv.conf && printf '# build-only resolver placeholder\n' > /etc/resolv.conf
RUN --mount=type=bind,source=os/scripts/clean-bootc-transient-state.py,target=/usr/local/libexec/tilo-clean-bootc-transient-state.py,ro \
    --mount=type=bind,source=os/usr/lib/systemd/system/ldconfig.service.d/10-tilo-image-cache.conf,target=/usr/local/libexec/tilo-ldconfig-image-cache.conf,ro \
    --mount=type=bind,source=os/scripts/verify-boot-ordering.sh,target=/usr/local/libexec/tilo-verify-boot-ordering.sh,ro \
    --mount=type=bind,source=os/scripts/pin-directory-times.py,target=/usr/local/libexec/tilo-pin-directory-times.py,ro \
    --mount=type=bind,source=os/scripts/seed-env-runtime.py,target=/usr/local/libexec/tilo-seed-env-runtime.py,ro \
    --mount=type=bind,from=native-builder-payload,source=/usr/share/tilo-apps,target=/usr/local/libexec/tilo-apps-built,ro \
    ! find /usr/lib /usr/lib64 \
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
    # The rpmdb finaliser was build scratch under /run; nothing runs it now.
    && rm -f /run/tilo/finalize-rpmdb \
    # The base image's tool for deriving base images, a Python program no
    # package owns; a derived image never runs it.
    && rm -f /usr/libexec/bootc-base-imagectl \
    && rm -f /usr/local/bin/verify-no-python-runtime.sh \
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
    # The system is Tilo: its name in the boot menu and on the console, and the
    # hostname a disk keeps until one is chosen. ID and VERSION_ID stay
    # Fedora's for the tools keyed off them. Edited after the package check,
    # as it changes a packaged file on purpose.
    && sed -i -e 's/^NAME=.*/NAME="Tilo"/' -e 's/^PRETTY_NAME=.*/PRETTY_NAME="Tilo"/' \
         -e 's/^DEFAULT_HOSTNAME=.*/DEFAULT_HOSTNAME="tilo"/' /usr/lib/os-release \
    && grep -qx 'PRETTY_NAME="Tilo"' /usr/lib/os-release \
    && grep -qx 'DEFAULT_HOSTNAME="tilo"' /usr/lib/os-release \
    && grep -qx 'ID=fedora' /usr/lib/os-release \
    && test "$(readlink -f /etc/os-release)" = /usr/lib/os-release \
    && test "$(bootc --version)" = "bootc $(rpm -q --qf '%{VERSION}' bootc)"

# COPY is a separate build operation, with no DNS mount in /run. Copy the
# containing directory so builders preserve its only entry, the symlink,
# instead of dereferencing a single-file source with an absent /run target.
# Restore the NetworkManager resolver before split/ukify seal the rootfs.
COPY --from=runtime-resolver-link /out/ /etc/

