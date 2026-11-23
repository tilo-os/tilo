#!/usr/bin/env bash
# Sign the Linux installer downloads of a release tag with the release key, in a
# keyring that exists only for this step, and publish the key's public half and
# fingerprint beside them for verify-linux-release.sh and the site.
#
#   sign.sh <arch> <package-dir> <out>
#
# TILO_LINUX_SIGNING_KEY_ASC: the armored secret key (a t secret).
# TILO_LINUX_EXPECTED_FINGERPRINT: the release key's full fingerprint.
set -euo pipefail

arch="${1:?x86_64 or aarch64}"
packages="${2:?the release/linux-<arch> output}"
out="${3:?output directory}"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
: "${TILO_LINUX_SIGNING_KEY_ASC:?the release key is not configured for this check}"
: "${TILO_LINUX_EXPECTED_FINGERPRINT:?the release key fingerprint is not configured for this check}"

GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
trap 'gpgconf --kill all >/dev/null 2>&1 || true; rm -rf -- "$GNUPGHOME"' EXIT
printf '%s\n' "$TILO_LINUX_SIGNING_KEY_ASC" | gpg --batch --quiet --import
export TILO_LINUX_SIGNING_KEY="$TILO_LINUX_EXPECTED_FINGERPRINT"

rm -rf "$out" && mkdir -p "$out"
for artifact in tar.gz AppImage flatpak; do
  name="tilo-imager-linux-$arch.$artifact"
  (cd "$packages" && sha256sum --check --strict --quiet "$name.sha256")
  cp -- "$packages/$name" "$packages/$name.sha256" "$out/"
  bash "$repo/apps/imager/packaging/sign-linux-release-file.sh" "$out/$name" "$out/$name.asc"
done
gpg --batch --armor --export "$TILO_LINUX_EXPECTED_FINGERPRINT" > "$out/tilo-release-key.asc"
printf '%s\n' "$TILO_LINUX_EXPECTED_FINGERPRINT" > "$out/tilo-release-key.fingerprint"
(cd "$out" && for file in tilo-release-key.asc tilo-release-key.fingerprint; do sha256sum "$file" > "$file.sha256"; done)
