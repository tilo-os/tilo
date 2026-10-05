#!/bin/bash
# Build the image's initramfs for KVER and check what dracut put in it.
#
#   build-initramfs.sh KVER
#
# Used by the rootfs build and by the dev image's board layer, so a board dev
# image boots from the same initramfs recipe as the released image.
#
# --add rather than trusting the base's drop-ins to pull these in: the same
# invocation produced an initramfs WITH the crypt modules locally and WITHOUT
# them under the CI builder, silently, and a missing module here is a root
# that cannot be unlocked. Naming them makes dracut fail instead of omit.
# Tilo installs only onto a local block device and supports passphrase,
# TPM2, FIDO2 and sibling unlock. The last one (tilo-sibling-unlock) carries
# the image's NetworkManager to reach the owner's other devices, but starts
# it only for a root whose LUKS2 header enrolled that factor; it parses no
# network-root command line and pulls in no network-root module. Network roots and network-bound disk encryption
# add DHCP/TLS/NFS/iSCSI parsers plus startup work to every signed UKI with
# no supported way to use them. The supported roots are Btrfs and Ext4;
# Ext4 must be available before pivot_root for an Ext4 installation. Keep
# XFS as a signed rescue/secondary-filesystem module after pivot_root.
# Omit the consumers as well as their parents: bootc explicitly adds
# clevis-pin-tpm2, and dracut can auto-select the other Clevis pins and
# network-root transports. Omitting only clevis/network leaves their
# dependencies unsatisfied. Local TPM unlock uses systemd-cryptsetup and
# tpm2-tss; local NVMe does not require the nvmf network-root module.
# The native GPU drivers whose firmware dominates the image (nouveau's GSP
# firmware alone is ~100 MB) stay out of every profile's initramfs: the
# built-in simpledrm draws plymouth and the passcode prompt, and the native
# driver loads from the root after switch-root. This keeps the UKI small
# enough to load and unpack quickly and to fit several generations on the ESP.
set -euo pipefail
kver="${1:?usage: build-initramfs.sh KVER}"

find "/usr/lib/modules/$kver" -type f -path '*/fs/ext4/ext4.ko*' -print -quit \
  | grep -q . || { echo 'signed Ext4 rescue module is missing' >&2; exit 1; }
find "/usr/lib/modules/$kver" -type f -path '*/fs/xfs/xfs.ko*' -print -quit \
  | grep -q . || { echo 'signed XFS rescue module is missing' >&2; exit 1; }
# ldconfig -r in dracut requires absolute include patterns. Fedora's
# default relative pattern resolves to the same directory without -r.
sed -i 's@^include ld\.so\.conf\.d/\*\.conf$@include /etc/ld.so.conf.d/*.conf@' /etc/ld.so.conf
# Build containers do not guarantee a writable /dev/log. Keep -v stderr
# diagnostics while disabling the distro's default syslog sink.
printf 'sysloglvl=0\n' > /etc/dracut.conf.d/99-tilo-build-logging.conf
omit_modules="network network-legacy network-manager nfs iscsi nvmf nbd fcoe fcoe-uefi nbde clevis clevis-pin-null clevis-pin-sss clevis-pin-tang clevis-pin-tpm2 crypt-ssh"
required_modules="crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback tilo-sibling-unlock"
log="$(mktemp /tmp/tilo-dracut.XXXXXX)"
trap 'rm -f -- "$log"' EXIT
dracut --force --reproducible --compress zstd -v --kver "$kver" \
    --omit "network network-legacy network-manager nfs iscsi nvmf nbd fcoe fcoe-uefi nbde clevis clevis-pin-null clevis-pin-sss clevis-pin-tang clevis-pin-tpm2 crypt-ssh" \
    --omit-drivers "amdgpu nouveau xe xfs" \
    --add-drivers ext4 \
    --add "crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback tilo-sibling-unlock" \
    "/usr/lib/modules/$kver/initramfs.img" > "$log" 2>&1 \
  || { cat "$log" >&2; exit 1; }
test -s "/usr/lib/modules/$kver/initramfs.img"
# Record what dracut says it included, and check THAT rather than reading the
# image back. dracut wraps the contents in a squashfs, and listing one needs
# privileges an unprivileged image builder does not have — so inspecting the
# artefact reports "no cryptsetup" for an initramfs that has it. dracut's own
# report is the same fact without the privilege dependency, and shipping the
# list lets the runtime check use it too.
mkdir -p /usr/lib/tilo
sed -n 's/.*Including module: \([a-z0-9_-]*\).*/\1/p' "$log" \
  | sort -u > /usr/lib/tilo/initramfs-modules.txt
printf '%s\n' amdgpu nouveau xe xfs > /usr/lib/tilo/initramfs-omitted-drivers.txt
for m in $required_modules; do
  grep -qx "$m" /usr/lib/tilo/initramfs-modules.txt || {
    echo "dracut did not include the $m module; an encrypted root could not be opened" >&2
    cat /usr/lib/tilo/initramfs-modules.txt >&2; exit 1; }
done
for m in $omit_modules; do
  ! grep -qx "$m" /usr/lib/tilo/initramfs-modules.txt || {
    echo "unsupported initramfs module $m was included" >&2; exit 1; }
done
# A board image (one with a hardware-profile definition) also reads its
# initramfs back: the profile's firmware and boot modules, and the Wi-Fi pieces
# sibling unlock installs only optionally. Generic images skip this.
python3 -B "$(dirname "${BASH_SOURCE[0]}")/verify-board-initramfs.py" "$kver"
