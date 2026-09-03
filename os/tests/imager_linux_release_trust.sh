#!/usr/bin/env bash
# Prove that Linux promotion accepts one pinned key and rejects tampering.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
verify="$repo/apps/imager/packaging/verify-linux-release.sh"
work="$(mktemp -d /var/tmp/qos-imager-linux-trust-test.XXXXXX)"
trap 'find "$work" -depth -delete' EXIT
export GNUPGHOME="$work/signing-keyring"
mkdir -m 0700 "$GNUPGHOME"
gpg --batch --passphrase '' --quick-generate-key \
  'qOS release trust test <release-test@qos.invalid>' ed25519 sign 1d >/dev/null 2>&1
fingerprint="$(gpg --batch --with-colons --fingerprint \
  | awk -F: '$1 == "pub" { want = 1; next }
               want && $1 == "fpr" { print toupper($10); exit }')"
assets="$work/assets"
mkdir -p "$assets/linux-x86_64" "$assets/linux-aarch64" "$assets/release-key"

for arch in x86_64 aarch64; do
  directory="$assets/linux-$arch"
  for suffix in AppImage tar.gz; do
    artifact="$directory/qos-imager-linux-$arch.$suffix"
    printf 'qOS Imager test artifact: %s %s\n' "$arch" "$suffix" >"$artifact"
    (cd "$directory" && sha256sum "$(basename "$artifact")" \
      >"$(basename "$artifact").sha256")
    gpg --batch --yes --armor --detach-sign "$artifact"
  done
done
gpg --batch --armor --export "$fingerprint" \
  >"$assets/release-key/qos-release-key.asc"
printf '%s\n' "$fingerprint" >"$assets/release-key/qos-release-key.fingerprint"
(cd "$assets/release-key" && sha256sum qos-release-key.asc \
  >qos-release-key.asc.sha256)
(cd "$assets/release-key" && sha256sum qos-release-key.fingerprint \
  >qos-release-key.fingerprint.sha256)

QOS_LINUX_EXPECTED_FINGERPRINT="$fingerprint" "$verify" "$assets" \
  | grep -Fq 'verified 4 Linux artifacts'

wrong="${fingerprint%?}0"
[ "$wrong" != "$fingerprint" ] || wrong="${fingerprint%?}1"
if QOS_LINUX_EXPECTED_FINGERPRINT="$wrong" "$verify" "$assets" \
   >"$work/wrong.out" 2>"$work/wrong.err"; then
  printf 'wrong protected fingerprint unexpectedly passed\n' >&2
  exit 1
fi
grep -Fq 'published Linux fingerprint does not match protected release policy' \
  "$work/wrong.err"

printf 'tampered\n' >>"$assets/linux-aarch64/qos-imager-linux-aarch64.AppImage"
if QOS_LINUX_EXPECTED_FINGERPRINT="$fingerprint" "$verify" "$assets" \
   >"$work/tamper.out" 2>"$work/tamper.err"; then
  printf 'tampered artifact unexpectedly passed\n' >&2
  exit 1
fi
grep -Eq 'FAILED|computed checksum did NOT match' "$work/tamper.err"

printf 'ok    Linux release trust bundle fails closed on policy mismatch and tampering\n'
