#!/usr/bin/env bash
# Bound the residual distribution OpenSSL ABI to reviewed Fedora base and desktop/VM dependencies.
# Tilo-owned TLS and signing code is checked separately at the ELF level and may
# never appear here. Run inside the finished rootfs after package removal.
set -euo pipefail
rpm_bin="${TILO_RPM_BIN:-rpm}"

# Named ABI boundary reviewed against the pinned Fedora 44 base and the resolved
# strong/weak dependency closure of rootfs package requests, excluding build-only roots and
# explicitly purged packages. This is NOT an allowance for every snapshot RPM.
# See docs/security/boot-chain.md for the feature and CLI-package boundaries.

allowed_libcrypto='bind-libs
bootc
chunkah
clevis
clevis-pin-tpm2
composefs-libs
coreutils
cryptsetup-libs
dracut
elfutils-debuginfod-client
freerdp-libs
glusterfs
glusterfs-client-xlators
glusterfs-fuse
gstreamer1-plugins-bad-free
gstreamer1-plugins-good
hiredis
ima-evm-utils-libs
iscsi-initiator-utils
kmod
kmod-libs
krb5-libs
libarchive
libcurl
libevent
libfido2
libfprint
libfsverity
libgfapi0
libgfrpc0
libgfxdr0
libglusterfs0
libimobiledevice
libjose
libnvme
librabbitmq
librados2
librbd1
librepo
libretls
libsrtp
libssh
libssh2
libsss_certmap
libtpms
libwinpr
libzip
ngtcp2-crypto-ossl
openldap
openssh
openssh-clients
openssl-libs
ostree
ostree-libs
pam
pipewire-libs
pkcs11-provider
podman-sequoia
python3-libs
qatlib
rpm-ostree
rpm-sequoia
socat
spice-glib
spice-server
sssd-ad
sssd-common
sssd-ipa
sssd-krb5
sssd-ldap
sudo
sudo-python-plugin
swtpm-libs
swtpm-tools
systemd
systemd-container
systemd-shared
systemd-udev
thrift
tpm2-tools
tpm2-tss
tpm2-tss-fapi
unbound-anchor
unbound-libs
wpa_supplicant
xorg-x11-server-Xwayland
zchunk-libs'
allowed_libssl='bind-libs
bootc
chunkah
clevis-pin-tpm2
freerdp-libs
glusterfs
gstreamer1-plugins-bad-free
hiredis
krb5-libs
libcurl
libevent
libimobiledevice
librabbitmq
librbd1
libretls
libssh2
libwinpr
ngtcp2-crypto-ossl
openldap
podman-sequoia
python3-libs
rpm-ostree
socat
spice-glib
spice-server
sssd-common
sudo
thrift
unbound-anchor
unbound-libs
wpa_supplicant'
forbidden_packages='efitools
git
git-core
libshout
mokutil
nss-tools
openvpn
openssh-server
openssl
openssl-devel
perl-IO-Socket-SSL
perl-Net-SSLeay
pesign
python3-dbus-next
python3-gobject
qatlib-service
yt-dlp
sbsigntools
systemd-resolved'

crypto_consumers="$(mktemp)"
ssl_consumers="$(mktemp)"
requirements="$(mktemp)"
trap 'rm -f "$crypto_consumers" "$ssl_consumers" "$requirements"' EXIT

# `=NAME` repeats the scalar package name inside REQUIRENAME's array iterator;
# without the equals marker RPM rejects the differently sized tag arrays.
"$rpm_bin" -qa --qf '[%{=NAME}\t%{REQUIRENAME}\n]' >"$requirements"
awk -F '\t' '$2 ~ /^libcrypto\.so/ { print $1 }' "$requirements" \
  | sort -u >"$crypto_consumers"
awk -F '\t' '$2 ~ /^libssl\.so/ { print $1 }' "$requirements" \
  | sort -u >"$ssl_consumers"

# Query names independently: a package with no REQUIRENAME rows must still be
# checked against the forbidden and conditional package policy.
installed="$("$rpm_bin" -qa --qf '%{NAME}\n' | sort -u)"
has_package() { grep -Fxq -- "$1" <<<"$installed"; }
require_parent() {
  local package="$1" parent="$2"
  if has_package "$package" && ! has_package "$parent"; then
    echo "runtime package $package is permitted only with $parent" >&2
    exit 1
  fi
}
# swtpm_localca uses certtool to bootstrap the VM's endorsement-certificate CA.
# virt-viewer has a strong Fedora dependency on OpenSSH clients, even though
# Tilo connects only to qemu:///system. Neither exception permits an SSH server
# or changes the separate prohibition on OpenSSL in Tilo-owned ELF outputs.
require_parent gnutls-utils swtpm-tools
require_parent openssh virt-viewer
require_parent openssh-clients virt-viewer
forbidden_installed="$(grep -Fx -f <(printf '%s\n' "$forbidden_packages") \
  <<<"$installed" || true)"
if [ -n "$forbidden_installed" ]; then
  echo 'forbidden crypto/signing packages remain in the finished rootfs:' >&2
  printf '%s\n' "$forbidden_installed" >&2
  exit 1
fi

unexpected_ssl="$(sort -u "$ssl_consumers" | grep -Fvx -f <(printf '%s\n' "$allowed_libssl") || true)"
if [ -n "$unexpected_ssl" ]; then
  echo 'unreviewed runtime packages directly requiring libssl:' >&2
  printf '%s\n' "$unexpected_ssl" >&2
  exit 1
fi

unexpected="$(sort -u "$crypto_consumers" | grep -Fvx -f <(printf '%s\n' "$allowed_libcrypto") || true)"
if [ -n "$unexpected" ]; then
  echo 'unreviewed runtime packages directly requiring libcrypto:' >&2
  printf '%s\n' "$unexpected" >&2
  exit 1
fi

echo 'reviewed residual libcrypto package boundary:'
sort -u "$crypto_consumers"
echo 'reviewed residual libssl package boundary:'
sort -u "$ssl_consumers"
