#!/usr/bin/env bash
# Bound the residual distribution OpenSSL ABI to reviewed foundational users.
# Tilo-owned TLS and signing code is checked separately at the ELF level and may
# never appear here. Run inside the finished rootfs after package removal.
set -euo pipefail
rpm_bin="${TILO_RPM_BIN:-rpm}"

allowed_libcrypto='composefs-libs
coreutils
cryptsetup-libs
kmod-libs
systemd
systemd-libs
tpm2-tools
tpm2-tss'
allowed_libssl='gstreamer1-plugins-bad-free
krb5-libs
libcurl
libevent
libshout
python3-libs
qt6-qtbase
rpm-sequoia
sudo'
forbidden_packages='efitools
git
git-core
gnutls-utils
mokutil
nss-tools
openvpn
openssl
openssl-devel
pesign
python3-dbus-next
python3-gobject
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

installed="$(cut -f1 "$requirements" | sort -u)"
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
