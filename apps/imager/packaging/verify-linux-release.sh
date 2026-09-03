#!/usr/bin/env bash
# Verify the complete Linux release set using only the public trust bundle that
# will be published beside it. This deliberately uses a fresh GnuPG home.
set -euo pipefail

root="${1:-release-assets}"
expected="${QOS_LINUX_EXPECTED_FINGERPRINT:-}"
expected="${expected^^}"
expected="${expected//[[:space:]]/}"
[[ "$expected" =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] \
  || { printf 'QOS_LINUX_EXPECTED_FINGERPRINT must be a full OpenPGP fingerprint\n' >&2; exit 1; }

find_one() {
  local name="$1"
  local -a matches
  mapfile -t matches < <(find "$root" -type f -name "$name" -print | sort)
  [ "${#matches[@]}" -eq 1 ] \
    || { printf 'expected exactly one %s in the release set, found %s\n' \
      "$name" "${#matches[@]}" >&2; exit 1; }
  printf '%s\n' "${matches[0]}"
}

verify_digest() {
  local artifact="$1" sidecar
  sidecar="$(find_one "$(basename "$artifact").sha256")"
  [ "$(dirname "$artifact")" = "$(dirname "$sidecar")" ] \
    || { printf 'digest sidecar is not adjacent to %s\n' "$(basename "$artifact")" >&2; exit 1; }
  (cd "$(dirname "$artifact")" && sha256sum --check --strict "$(basename "$sidecar")" >/dev/null)
}

key="$(find_one qos-release-key.asc)"
fingerprint_file="$(find_one qos-release-key.fingerprint)"
verify_digest "$key"
verify_digest "$fingerprint_file"

published_fingerprint="$(tr -d '[:space:]' <"$fingerprint_file")"
[ "$published_fingerprint" = "$expected" ] \
  || { printf 'published Linux fingerprint does not match protected release policy\n' >&2; exit 1; }

keyring="$(mktemp -d /var/tmp/qos-imager-release-keyring.XXXXXX)"
trap 'find "$keyring" -depth -delete' EXIT
chmod 0700 "$keyring"
GNUPGHOME="$keyring" gpg --batch --import "$key" >/dev/null 2>&1
if GNUPGHOME="$keyring" gpg --batch --with-colons --list-secret-keys 2>/dev/null \
   | grep -q '^sec:'; then
  printf 'published Linux release key unexpectedly contains private key material\n' >&2
  exit 1
fi
mapfile -t imported_fingerprints < <(
  GNUPGHOME="$keyring" gpg --batch --with-colons --fingerprint \
    | awk -F: '$1 == "pub" { want = 1; next }
                 want && $1 == "fpr" { print toupper($10); want = 0 }'
)
[ "${#imported_fingerprints[@]}" -eq 1 ] \
  && [ "${imported_fingerprints[0]}" = "$expected" ] \
  || { printf 'published Linux key does not contain exactly the expected public identity\n' >&2; exit 1; }

verified=0
for arch in x86_64 aarch64; do
  for suffix in AppImage tar.gz; do
    artifact="$(find_one "qos-imager-linux-$arch.$suffix")"
    signature="$(find_one "$(basename "$artifact").asc")"
    [ "$(dirname "$artifact")" = "$(dirname "$signature")" ] \
      || { printf 'signature is not adjacent to %s\n' "$(basename "$artifact")" >&2; exit 1; }
    verify_digest "$artifact"
    identity="$(GNUPGHOME="$keyring" gpg --batch --status-fd 1 \
      --verify "$signature" "$artifact" 2>/dev/null \
      | awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" { print $3 " " $NF; exit }')"
    read -r signing_fingerprint primary_fingerprint <<<"$identity"
    if [ "$signing_fingerprint" != "$expected" ] \
       && [ "$primary_fingerprint" != "$expected" ]; then
      printf 'signature for %s was not made by expected release key %s\n' \
        "$(basename "$artifact")" "$expected" >&2
      exit 1
    fi
    verified=$((verified + 1))
  done
done

[ "$verified" -eq 4 ] \
  || { printf 'did not verify the complete Linux release matrix\n' >&2; exit 1; }
printf 'verified %s Linux artifacts with release key %s\n' "$verified" "$expected"
