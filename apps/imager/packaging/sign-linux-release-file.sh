#!/usr/bin/env bash
# Sign one release payload with the same independently pinned OpenPGP identity
# used for the Linux Imager packages.
set -euo pipefail

artifact="${1:?usage: sign-linux-release-file.sh ARTIFACT [SIGNATURE]}"
signature="${2:-$artifact.asc}"
expected="${TILO_LINUX_EXPECTED_FINGERPRINT:-}"
expected="${expected^^}"
expected="${expected//[[:space:]]/}"
[[ "$expected" =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] \
  || { printf 'TILO_LINUX_EXPECTED_FINGERPRINT must be a full OpenPGP fingerprint\n' >&2; exit 1; }
[ -s "$artifact" ] || { printf 'release payload is missing: %s\n' "$artifact" >&2; exit 1; }
[ ! -e "$signature" ] || { printf 'refusing to replace release signature: %s\n' "$signature" >&2; exit 1; }

selector="${TILO_LINUX_VERIFY_KEY:-${TILO_LINUX_SIGNING_KEY:-}}"
[ -n "$selector" ] || { printf 'no Linux release-key selector was configured\n' >&2; exit 1; }
release_gpg=(gpg --batch)
verify_home=
cleanup() {
  if [ -n "$verify_home" ]; then
    gpgconf --homedir "$verify_home" --kill all >/dev/null 2>&1 || true
    rm -rf -- "$verify_home"
  fi
}
trap cleanup EXIT
if [ -n "${TILO_LINUX_VERIFY_KEY:-}" ]; then
  [ -f "$TILO_LINUX_VERIFY_KEY" ] && [ ! -L "$TILO_LINUX_VERIFY_KEY" ] \
    || { printf 'TILO_LINUX_VERIFY_KEY must be a regular public-key file\n' >&2; exit 1; }
  mapfile -t fingerprints < <(
    gpg --batch --with-colons --show-keys --fingerprint \
      "$TILO_LINUX_VERIFY_KEY" 2>/dev/null \
      | awk -F: '$1 == "pub" { want = 1; next }
                   want && $1 == "fpr" { print toupper($10); want = 0 }'
  )
else
  mapfile -t fingerprints < <(
    gpg --batch --with-colons --fingerprint "$selector" 2>/dev/null \
      | awk -F: '$1 == "pub" { want = 1; next }
                   want && $1 == "fpr" { print toupper($10); want = 0 }'
  )
fi
[ "${#fingerprints[@]}" -eq 1 ] && [ "${fingerprints[0]}" = "$expected" ] \
  || { printf 'Linux release key does not match protected release policy\n' >&2; exit 1; }

if [ -n "${TILO_LINUX_VERIFY_KEY:-}" ]; then
  verify_home="$(mktemp -d "${TMPDIR:-/tmp}/tilo-imager-verify.XXXXXX")"
  chmod 0700 "$verify_home"
  gpg --batch --homedir "$verify_home" --import-options import-minimal \
    --import "$TILO_LINUX_VERIFY_KEY" >/dev/null 2>&1 \
    || { printf 'could not import the Linux release verification key\n' >&2; exit 1; }
  release_gpg=(gpg --batch --homedir "$verify_home")
fi

if [ -n "${TILO_LINUX_SIGNER:-}" ]; then
  "$TILO_LINUX_SIGNER" "$artifact" "$signature"
elif [ -n "${TILO_LINUX_SIGNING_KEY:-}" ]; then
  gpg --batch --yes --local-user "$TILO_LINUX_SIGNING_KEY" \
    --armor --detach-sign --output "$signature" "$artifact"
else
  printf 'Linux signing credentials are required\n' >&2
  exit 1
fi
[ -s "$signature" ] || { printf 'Linux signer did not create %s\n' "$signature" >&2; exit 1; }

identity="$("${release_gpg[@]}" --status-fd 1 --verify "$signature" "$artifact" 2>/dev/null \
  | awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" { print $3 " " $NF; exit }')"
read -r signing_fingerprint primary_fingerprint <<<"$identity"
if [ "$signing_fingerprint" != "$expected" ] \
   && [ "$primary_fingerprint" != "$expected" ]; then
  printf 'signature for %s was not made by expected release key %s\n' \
    "$(basename "$artifact")" "$expected" >&2
  exit 1
fi
