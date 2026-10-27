# The kernel is a package transaction of its own, over the packages: keyed by
# the pinned kernel image and the RPM snapshot, it doesn't rerun when Tilo's
# code or configuration changes. Its initramfs reads Tilo's binaries (the
# rollback and unlock helpers it carries) and the public trust, so it is built
# above them, with the derived state (rootfs.Containerfile).
FROM packages AS kernel-installed
# The build-only dnf/rpm wrappers read it.
ARG SOURCE_DATE_EPOCH=0
ARG FINALIZE_RPMDB="/usr/libexec/tilo/finalize-rpmdb"
ARG RPMS_REF

# ---------------------------------------------------------------------------
# Custom Tilo kernel: swap Fedora's kernel-core for our tuned tilo-kernel-core.
# ---------------------------------------------------------------------------
# The kernel image (see os/kernel/Containerfile and
# tools/ci/publish/kernel.sh) rebuilds Fedora's kernel with a config fragment that:
#   * links storage/USB/HID boot-path drivers statically while keeping network
#     drivers signed and modular because networking is off the login path, so the
#     boot path needs no module loading to reach the root device, and
#   * tunes for interactive latency (PREEMPT + HZ=1000 + THP=madvise +
#     SCHED_MC), and
#   * drops DEBUG_INFO so the RPM doesn't carry ~800 MB of debuginfo.
#
# Because the tilo-kernel repo is a distinct OCI image (KERNEL_RPMS_REF),
# we mount it alongside the frozen repo, write a two-repo dnf config on
# the fly, and swap kernel-core → tilo-kernel-core.  --nogpgcheck: our
# in-house RPMs aren't signed by Fedora's key. The image build verifies the
# digest-pinned tilo-kernel image's cosign signature and provenance before
# consuming it (os/dev/verify-kernel-provenance.sh);
# os/etc/containers/policy.json has no tilo-kernel scope, and an installed
# machine never pulls that image.
#
# An initramfs is mandatory, and the static drivers do not remove the need
# for one: unlocking an encrypted root takes early USERSPACE (device-mapper
# setup, a TPM unseal or a passphrase prompt, systemd-cryptsetup). Static
# drivers get the kernel as far as seeing the block device; they cannot open
# a LUKS volume. Under the base's `layout=ostree` kernel-install config,
# kernel-install deliberately does not run dracut, so the image build owns
# initramfs generation — hence the explicit dracut call in rootfs.Containerfile,
# once everything the initramfs carries is in place. The base's
# dracut drop-ins (/usr/lib/dracut/dracut.conf.d/*bootc*) already add the
# crypt, systemd-cryptsetup, tpm2-tss and clevis modules.
ARG TILO_HARDWARE_PROFILE=generic
# Buildah does not hash a `--mount=from=` stage into this RUN's cache key, so
# a changed `FROM ${KERNEL_RPMS_REF} AS kernel-rpms` reused the previous swap
# and the new kernel was never installed. Redeclaring the ref here (it is
# already a build-arg) puts its value in this RUN's cache key only; layers
# before this ARG stay cached. The shell below reads it, so the layer reruns
# when the kernel image identity changes.
ARG KERNEL_RPMS_REF
# The Lenovo profile needs the Adreno SQE firmware from Fedora's qcom-firmware
# RPM. Keep it in the frozen ARM package set, but install it only in that image.
# tilo-rpm-snapshot-package[aarch64]: qcom-firmware
# Hash only this profile's firmware. Later board imports must not rebuild the
# generic kernel/initramfs when the runner seed publishes the same rootfs.
RUN --mount=type=bind,from=rpms,source=/repo,target=/frozen-repo \
    --mount=type=bind,from=kernel-rpms,source=/repo,target=/qkernel-repo \
    set -eux; \
    test -n "$KERNEL_RPMS_REF"; \
    mkdir -p /etc/tilo-swap.d; \
    printf '[frozen]\nname=frozen\nbaseurl=file:///frozen-repo\ngpgcheck=0\nenabled=1\n' \
        > /etc/tilo-swap.d/frozen.repo; \
    printf '[tilo-kernel]\nname=tilo-kernel\nbaseurl=file:///qkernel-repo\ngpgcheck=0\nenabled=1\n' \
        > /etc/tilo-swap.d/tilo-kernel.repo; \
    if [ "$TILO_HARDWARE_PROFILE" = lenovo-83ed ]; then \
        dnf install -y --setopt=reposdir=/etc/tilo-swap.d qcom-firmware; \
    fi; \
    # --allowerasing lets the swap drop the base's `kernel` meta-package
    # (it hard-requires kernel-core-uname-r = <exact-fedora-version>,
    # which our tilo-kernel-core doesn't satisfy since it carries a
    # different Release tag). tilo-kernel-core Provides: kernel so nothing
    # downstream loses a dependency.
    dnf swap -y --setopt=reposdir=/etc/tilo-swap.d --nogpgcheck --allowerasing \
        kernel-core tilo-kernel-core; \
    rm -rf /etc/tilo-swap.d; \
    # Which kernel version did the swap leave behind? Ask the filesystem: rpm is
    # no help here, because tilo-kernel-core provides no kernel-uname-r and its
    # file list records /lib/modules/... where a query would say /usr/lib/... .
    #
    # Count kernel IMAGES, not directories. Removing kernel-core does not remove
    # its modules directory: the base ships a prebuilt initramfs there that no
    # package owns, so the directory survives with that file in it and a count of
    # directories finds two. The loop further down exists to clear exactly that
    # leftover — so asserting on directories here is the cleanup firing too late
    # rather than a real ambiguity, and it failed every build that did not restore
    # this layer from cache.
    #
    # A kernel image is /usr/lib/modules/<kver>/vmlinuz (Fedora's layout) or
    # /boot/vmlinuz-<kver> (tilo-kernel-core's, moved into place below). The
    # leftover directory has neither, because its vmlinuz belonged to the package
    # that was just removed.
    kver=""; n=0; \
    for d in /usr/lib/modules/*/; do \
      k="$(basename "$d")"; \
      if [ -f "$d/vmlinuz" ] || [ -f "/boot/vmlinuz-$k" ]; then \
        kver="$k"; n=$((n+1)); \
      fi; \
    done; \
    if [ "$n" -ne 1 ]; then \
      echo "expected exactly one kernel image after the kernel swap, found $n" >&2; \
      ls -la /usr/lib/modules/ /boot >&2 || true; \
      exit 1; \
    fi; \
    # tilo-kernel-core installs its kernel as /boot/vmlinuz-<kver>, not into the
    # modules directory the way Fedora's kernel-core does. ostree does find it
    # there — it falls back to /boot/vmlinuz-<kver> — which is why nobody noticed,
    # but it means the image carried a kernel and no initramfs beside it, and
    # every tool that goes looking for the pair (this build included) came up
    # empty. Put both where the base image puts them.
    if [ -f "/boot/vmlinuz-$kver" ] && [ ! -f "/usr/lib/modules/$kver/vmlinuz" ]; then \
      mv "/boot/vmlinuz-$kver" "/usr/lib/modules/$kver/vmlinuz"; \
    fi; \
    test -f "/usr/lib/modules/$kver/vmlinuz"; \
    # The base ships a prebuilt initramfs for ITS kernel, and that file is
    # owned by no RPM — so removing kernel-core leaves the old modules
    # directory behind and the image would carry two kernel versions.
    for d in /usr/lib/modules/*; do \
      [ "$d" = "/usr/lib/modules/$kver" ] || rm -rf "$d"; \
    done; \
    # /boot belongs to the deployment, not the image: whatever the kernel
    # packages dropped there is now duplicated under the modules directory.
    rm -f /boot/vmlinuz-* /boot/initramfs-* /boot/System.map-* /boot/config-*; \
    dnf clean all && ${FINALIZE_RPMDB}

# The rpm database this image ships is intact and carries no WAL sidecars.
#
# Asserted positively rather than by absence of an error, because that is how
# this failed before: `rpm -qa` kept working off the Packages blob while the
# Basenames/Dirnames indexes were structurally corrupt, so every step still
# "succeeded" until a dnf resolve needed a file dependency and claimed the base
# image did not provide /usr/bin/basename. `rpm -qf` on a file the base owns is
# the cheapest query that actually reads those indexes.
RUN owner="$(rpm -qf /usr/bin/basename 2>&1 || true)" \
 && case "$owner" in \
      coreutils-*) echo "rpmdb file index intact: /usr/bin/basename -> $owner" ;; \
      *) echo "rpmdb file index is corrupt: /usr/bin/basename -> $owner" >&2; exit 1 ;; \
    esac \
 && rpm --verifydb \
 && rm -f /usr/lib/sysimage/rpm/rpmdb.sqlite-wal \
           /usr/lib/sysimage/rpm/rpmdb.sqlite-shm \
 && ( test ! -e /usr/lib/sysimage/rpm/rpmdb.sqlite-wal \
        || { echo "rpmdb.sqlite-wal is in the image; see FINALIZE_RPMDB" >&2; exit 1; } ) \
 && ( test ! -e /usr/lib/sysimage/rpm/rpmdb.sqlite-shm \
        || { echo "rpmdb.sqlite-shm is in the image; see FINALIZE_RPMDB" >&2; exit 1; } )
