#!/bin/bash
# A/B verification helper: revert the running Tilo system to Fedora's stock
# kernel-core so you can boot-measure the SAME image with two different
# kernels (tilo-kernel vs. Fedora's default) and compare.
#
# What it does, and why:
#
#   1. `bootc usr-overlay` — the OS image ships /usr read-only.  This drops
#      an overlayfs on top so `dnf swap` can rewrite kernel packages inside
#      /usr for the CURRENT boot.  The change does NOT survive a
#      `bootc upgrade` — the next fetched image re-applies tilo-kernel.
#      For a permanent revert, edit os/Containerfile to remove the swap
#      block and rebuild.
#
#   2. `dnf swap tilo-kernel-core kernel-core` — swaps the metapackage back.
#      The Fedora kernel-core rpm is available from the frozen repo
#      (fingerprint-resolved RPMS_REF) — the swap resolves against it.  --nogpgcheck
#      because neither our RPMs nor the frozen-repo mirror carry the fedora
#      RPM signature; the image is signed as a whole via cosign upstream.
#
#   3. Build an initramfs for the swapped-in kernel.  Under the base's
#      `layout=ostree` config, kernel-install deliberately leaves initrd
#      generation to whoever assembles the image, so nothing produces one
#      here either — and without one the kernel cannot open an encrypted
#      root, or load any module the tilo-kernel had built in.
#
#   4. Suggest reboot.
#
# Undo: `bootc rollback` (reboots into the previous deployment, which
# still has tilo-kernel), OR reboot into the current deployment — the
# overlay is discarded and tilo-kernel is back.

set -euo pipefail

need_root() {
    if [ "$(id -u)" -ne 0 ]; then
        echo "run as root (sudo)" >&2
        exit 1
    fi
}
need_root

if ! command -v bootc >/dev/null 2>&1; then
    echo "bootc not found — this script is for the installed Tilo bootc system, not for the build container." >&2
    exit 1
fi

echo "[1/4] Making /usr writable for this boot (bootc usr-overlay)..."
bootc usr-overlay

echo "[2/4] Swapping tilo-kernel-core → kernel-core..."
dnf swap -y --nogpgcheck tilo-kernel-core kernel-core

echo "[3/4] Building an initramfs for the swapped-in kernel..."
# Same incantation os/Containerfile uses after its kernel swap: the ostree layout
# means kernel-install runs no dracut, so the initramfs is built explicitly and
# written next to the kernel where ostree looks for it.
for d in /usr/lib/modules/*; do
    [ -d "$d" ] || continue
    kver="$(basename "$d")"
    [ -f "$d/vmlinuz" ] || continue
    [ -s "$d/initramfs.img" ] && { echo "     $kver already has an initramfs"; continue; }
    dracut --force --reproducible --kver "$kver" "$d/initramfs.img"
    echo "     built $d/initramfs.img"
done

echo "[4/4] Done.  Reboot to boot the stock Fedora kernel."
echo "     (bootc rollback | reboot into the previous deployment to return to tilo-kernel)"
