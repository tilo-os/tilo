#!/usr/bin/env bash
# Verify the complete Linux release set using only the public trust bundle that
# will be published beside it. This deliberately uses a fresh GnuPG home.
set -euo pipefail

root="${1:-release-assets}"
expected="${TILO_LINUX_EXPECTED_FINGERPRINT:-}"
expected="${expected^^}"
expected="${expected//[[:space:]]/}"
[[ "$expected" =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] \
  || { printf 'TILO_LINUX_EXPECTED_FINGERPRINT must be a full OpenPGP fingerprint\n' >&2; exit 1; }

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

key="$(find_one tilo-release-key.asc)"
fingerprint_file="$(find_one tilo-release-key.fingerprint)"
verify_digest "$key"
verify_digest "$fingerprint_file"

published_fingerprint="$(tr -d '[:space:]' <"$fingerprint_file")"
[ "$published_fingerprint" = "$expected" ] \
  || { printf 'published Linux fingerprint does not match protected release policy\n' >&2; exit 1; }

keyring="$(mktemp -d /var/tmp/tilo-imager-release-keyring.XXXXXX)"
cleanup_keyring() {
  local verification_status=$? cleanup_status=0
  trap - EXIT
  # This is the exact private directory allocated above, never the caller's
  # signing keyring. Refuse an unexpected path before recursive removal.
  if [[ ! $keyring =~ ^/var/tmp/tilo-imager-release-keyring\.[[:alnum:]]{6}$ \
        || -L $keyring ]]; then
    printf 'refusing unsafe release keyring cleanup: %s\n' "$keyring" >&2
    cleanup_status=1
  else
    GNUPGHOME="$keyring" gpgconf --homedir "$keyring" --kill all || {
      cleanup_status=$?
      printf 'could not stop release verification agent: %s\n' "$keyring" >&2
    }
    # Agent shutdown can unlink sockets after directory enumeration. Ignore
    # only those disappeared entries, while preserving other deletion errors.
    find "$keyring" -ignore_readdir_race -depth -delete || {
      cleanup_status=$?
      printf 'could not remove release verification keyring: %s\n' "$keyring" >&2
    }
  fi
  if (( verification_status != 0 )); then
    exit "$verification_status"
  fi
  exit "$cleanup_status"
}
trap cleanup_keyring EXIT
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
verify_artifact() {
    local artifact="$1" signature identity signing_fingerprint primary_fingerprint
    signature="$(find_one "$(basename "$artifact").asc")"
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
}

# Authenticate the exact selected catalog before trusting its media inventory.
selector="$(dirname "${BASH_SOURCE[0]}")/release-catalog.py"
catalog="$(python3 "$selector" "$root")"
verify_artifact "$catalog"
media_list="$(python3 "$selector" "$root" --media-root "$root")"
while IFS= read -r artifact; do
  verify_artifact "$artifact"
done <<<"$media_list"

for name in \
  tilo-imager-linux-x86_64.AppImage \
  tilo-imager-linux-x86_64.tar.gz \
  tilo-imager-linux-x86_64.flatpak \
  tilo-imager-linux-aarch64.AppImage \
  tilo-imager-linux-aarch64.tar.gz \
  tilo-imager-linux-aarch64.flatpak; do
    verify_artifact "$(find_one "$name")"
done

# Current migration-enabled packages publish a canonical host-platform archive
# too. Historical fresh-install releases remain verifiable without that asset.
while IFS= read -r -d '' artifact; do
  verify_artifact "$artifact"
done < <(find "$root" -type f -name 'tilo-imager-linux-*-platform.tar.gz' -print0)

printf 'verified %s Linux and installer artifacts with release key %s\n' \
  "$verified" "$expected"
