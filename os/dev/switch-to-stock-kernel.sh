#!/bin/bash
# A/B verification helper: revert the running qOS system to Fedora's stock
# kernel-core so you can boot-measure the SAME image with two different
# kernels (qos-kernel vs. Fedora's default) and compare.
#
# What it does, and why:
#
#   1. `bootc usr-overlay` — the OS image ships /usr read-only.  This drops
#      an overlayfs on top so `dnf swap` can rewrite kernel packages inside
#      /usr for the CURRENT boot.  The change does NOT survive a
#      `bootc upgrade` — the next fetched image re-applies qos-kernel.
#      For a permanent revert, edit os/Containerfile to remove the swap
#      block and rebuild.
#
#   2. `dnf swap qos-kernel-core kernel-core` — swaps the metapackage back.
#      The Fedora kernel-core rpm is available from the frozen repo
#      (os/rpms-snapshot.env) — the swap resolves against it.  --nogpgcheck
#      because neither our RPMs nor the frozen-repo mirror carry the fedora
#      RPM signature; the image is signed as a whole via cosign upstream.
#
#   3. Re-enable initrd generation: the qos-kernel expects to boot without
#      one, but a swapped-in Fedora kernel needs dracut.  Edit
#      /etc/kernel/install.conf, then re-run `kernel-install add` for the
#      newly-swapped kernel version so /boot gets an initramfs.
#
#   4. Suggest reboot.
#
# Undo: `bootc rollback` (reboots into the previous deployment, which
# still has qos-kernel), OR reboot into the current deployment — the
# overlay is discarded and qos-kernel is back.

set -euo pipefail

need_root() {
    if [ "$(id -u)" -ne 0 ]; then
        echo "run as root (sudo)" >&2
        exit 1
    fi
}
need_root

if ! command -v bootc >/dev/null 2>&1; then
    echo "bootc not found — this script is for the installed qOS bootc system, not for the build container." >&2
    exit 1
fi

echo "[1/4] Making /usr writable for this boot (bootc usr-overlay)..."
bootc usr-overlay

echo "[2/4] Swapping qos-kernel-core → kernel-core..."
dnf swap -y --nogpgcheck qos-kernel-core kernel-core

echo "[3/4] Re-enabling initrd generation (Fedora kernel expects one)..."
if [ -f /etc/kernel/install.conf ]; then
    sed -i -e 's/^initrd_generator=none/initrd_generator=dracut/' \
           -e 's/^uki_generator=none/#uki_generator=none/' \
           /etc/kernel/install.conf
fi
# Regenerate for the currently-installed Fedora kernel(s).
for kver in /lib/modules/*; do
    [ -d "$kver" ] || continue
    kver="$(basename "$kver")"
    if [ -f "/lib/modules/$kver/vmlinuz" ]; then
        kernel-install add "$kver" "/lib/modules/$kver/vmlinuz" || true
    fi
done

echo "[4/4] Done.  Reboot to boot the stock Fedora kernel."
echo "     (bootc rollback | reboot into the previous deployment to return to qos-kernel)"
