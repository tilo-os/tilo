#!/usr/bin/env bash
# Build an enforcing x86 OVMF variable store which trusts the exact public
# certificate used to sign the release UKIs. No signing key is handled here.
set -euo pipefail

[ "$#" -eq 4 ] || {
  echo "usage: $0 OVMF_VARS_QCOW2 OUTPUT_QCOW2 SIGNER EXPECTED_CERT_SHA256" >&2
  exit 2
}
template="$1"
output="$2"
signer="$3"
expected="$4"

[ -f "$template" ] && [ ! -L "$template" ] \
  || { echo "OVMF variable-store template must be a regular file" >&2; exit 2; }
[[ "$output" = /* ]] \
  || { echo "output variable-store path must be absolute" >&2; exit 2; }
[[ "$signer" = /* ]] && [ -x "$signer" ] && [ ! -L "$signer" ] \
  || { echo "release signer must be an absolute executable non-symlink" >&2; exit 2; }
[[ "$expected" =~ ^[0-9a-f]{64}$ ]] \
  || { echo "expected release EFI certificate SHA-256 is invalid" >&2; exit 2; }
for tool in qemu-img tilo-crypto virt-fw-vars; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done

umask 077
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/var/tmp}}/tilo-release-vars.XXXXXX")"
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT

"$signer" export-efi-cert "$work/release-efi.crt"
[ -f "$work/release-efi.crt" ] && [ ! -L "$work/release-efi.crt" ] \
  || { echo "release signer did not export a regular EFI certificate" >&2; exit 1; }
actual="$(tilo-crypto certificate-sha256 "$work/release-efi.crt")" \
  || { echo "release signer did not export a valid X.509 certificate" >&2; exit 1; }
[ "$actual" = "$expected" ] \
  || { echo "release EFI certificate does not match protected fingerprint" >&2; exit 1; }

# Fedora's measured 4 MiB OVMF pair uses qcow2 pflash. virt-fw-vars edits the
# standard raw edk2 volume, so convert in and out without ever modifying the
# distribution-owned template.
qemu-img convert -f qcow2 -O raw "$template" "$work/blank.raw"
virt-fw-vars --input "$work/blank.raw" --output "$work/enrolled.raw" \
  --enroll-cert "$work/release-efi.crt" \
  --add-db a0baa8a3-041d-48a8-bc87-c36d121b5e3d "$work/release-efi.crt" \
  --microsoft-kek none --no-microsoft --secure-boot
qemu-img convert -f raw -O qcow2 "$work/enrolled.raw" "$work/enrolled.qcow2"
qemu-img check -f qcow2 "$work/enrolled.qcow2" >/dev/null

install -d -m 0700 "$(dirname "$output")"
install -m 0600 "$work/enrolled.qcow2" "$output.tmp"
mv -f -- "$output.tmp" "$output"
printf 'release-secureboot-vars=%s\nefi-certificate-sha256=%s\n' "$output" "$actual"
