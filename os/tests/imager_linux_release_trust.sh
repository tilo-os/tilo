#!/usr/bin/env bash
# Prove that Linux promotion accepts one pinned key and rejects tampering.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
verify="$repo/apps/imager/packaging/verify-linux-release.sh"
sign="$repo/apps/imager/packaging/sign-linux-release-file.sh"
work="$(mktemp -d /var/tmp/qos-imager-linux-trust-test.XXXXXX)"
export GNUPGHOME="$work/signing-keyring"
cleanup() {
  # GnuPG may remove its agent sockets while find is walking the temporary
  # keyring. Stop the agent first, then make best-effort cleanup explicitly
  # non-authoritative: trust-test failures come from the assertions above, not
  # an already-vanished socket in this disposable directory.
  gpgconf --homedir "$GNUPGHOME" --kill all >/dev/null 2>&1 || true
  find "$work" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT
mkdir -m 0700 "$GNUPGHOME"
gpg --batch --passphrase '' --quick-generate-key \
  'qOS release trust test <release-test@qos.invalid>' ed25519 sign 1d >/dev/null 2>&1
fingerprint="$(gpg --batch --with-colons --fingerprint \
  | awk -F: '$1 == "pub" { want = 1; next }
               want && $1 == "fpr" { print toupper($10); exit }')"
assets="$work/assets"
mkdir -p "$assets/linux-x86_64" "$assets/linux-aarch64" \
  "$assets/installer-x86_64" "$assets/installer-aarch64" \
  "$assets/catalog" "$assets/release-key" "$assets/installer-signatures"

for arch in x86_64 aarch64; do
  directory="$assets/linux-$arch"
  for suffix in AppImage tar.gz flatpak; do
    artifact="$directory/qos-imager-linux-$arch.$suffix"
    printf 'qOS Imager test artifact: %s %s\n' "$arch" "$suffix" >"$artifact"
    (cd "$directory" && sha256sum "$(basename "$artifact")" \
      >"$(basename "$artifact").sha256")
    gpg --batch --yes --armor --detach-sign "$artifact"
  done
done
for arch in x86_64 aarch64; do
  directory="$assets/installer-$arch"
  artifact="$directory/qos-installer-linux-$arch.img.zst"
  printf 'qOS installer test artifact: %s\n' "$arch" >"$artifact"
  (cd "$directory" && sha256sum "$(basename "$artifact")" \
    >"$(basename "$artifact").sha256")
  gpg --batch --yes --armor --detach-sign \
    --output "$assets/installer-signatures/$(basename "$artifact").asc" "$artifact"
done
catalog="$assets/catalog/qos-installer-catalog-v1.json"
printf '{"schema":1}\n' >"$catalog"
(cd "$assets/catalog" && sha256sum "$(basename "$catalog")" \
  >"$(basename "$catalog").sha256")
QOS_LINUX_SIGNING_KEY="$fingerprint" \
QOS_LINUX_EXPECTED_FINGERPRINT="$fingerprint" \
  "$sign" "$catalog" \
    "$assets/installer-signatures/$(basename "$catalog").asc"
gpg --batch --armor --export "$fingerprint" \
  >"$assets/release-key/qos-release-key.asc"
printf '%s\n' "$fingerprint" >"$assets/release-key/qos-release-key.fingerprint"
(cd "$assets/release-key" && sha256sum qos-release-key.asc \
  >qos-release-key.asc.sha256)
(cd "$assets/release-key" && sha256sum qos-release-key.fingerprint \
  >qos-release-key.fingerprint.sha256)

QOS_LINUX_EXPECTED_FINGERPRINT="$fingerprint" "$verify" "$assets" \
  | grep -Fq 'verified 9 Linux and installer artifacts'

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
