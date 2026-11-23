#!/usr/bin/env bash
# Sign the release medium's payload with the release key, in a keyring that exists only for this
# step, into the directory the release gathers a tag's media from.
#
#   sign-medium.sh <arch> <medium-dir> <out>
#
# TILO_LINUX_SIGNING_KEY_ASC: the armored secret key (a t secret).
# TILO_LINUX_EXPECTED_FINGERPRINT: the release key's full fingerprint.
#
# The medium's own bytes are checked against the digest the medium check wrote before anything is
# signed, and nothing unsigned is copied: OUT holds the payload, that digest, the catalog entry
# that names it, and the payload's detached signature.
set -euo pipefail

arch="${1:?x86_64 or aarch64}"
medium="${2:?the medium/imager-<arch> output}"
out="${3:?output directory}"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
: "${TILO_LINUX_SIGNING_KEY_ASC:?the release key is not configured for this check}"
: "${TILO_LINUX_EXPECTED_FINGERPRINT:?the release key fingerprint is not configured for this check}"

name="tilo-installer-linux-$arch.img.zst"
catalog="catalog-$arch.json"
(cd "$medium" && sha256sum --check --strict --quiet "$name.sha256")
[[ $(jq -er '.archive_sha256' "$medium/$catalog") == "$(sha256sum "$medium/$name" | awk '{print $1}')" ]] \
  || { printf 'the catalog names other bytes than the payload it ships\n' >&2; exit 1; }

GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
trap 'gpgconf --kill all >/dev/null 2>&1 || true; rm -rf -- "$GNUPGHOME"' EXIT
printf '%s\n' "$TILO_LINUX_SIGNING_KEY_ASC" | gpg --batch --quiet --import
export TILO_LINUX_SIGNING_KEY="$TILO_LINUX_EXPECTED_FINGERPRINT"

rm -rf "$out" && mkdir -p "$out"
cp -- "$medium/$name" "$medium/$name.sha256" "$medium/$catalog" "$medium/$catalog.sha256" "$out/"
bash "$repo/apps/imager/packaging/sign-linux-release-file.sh" "$out/$name" "$out/$name.asc"
printf 'signed %s for %s\n' "$name" "$arch"
