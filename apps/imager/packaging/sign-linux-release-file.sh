#!/usr/bin/env bash
# Sign one release payload with the same independently pinned OpenPGP identity
# used for the Linux Imager packages.
set -euo pipefail

artifact="${1:?usage: sign-linux-release-file.sh ARTIFACT [SIGNATURE]}"
signature="${2:-$artifact.asc}"
expected="${QOS_LINUX_EXPECTED_FINGERPRINT:-}"
expected="${expected^^}"
expected="${expected//[[:space:]]/}"
[[ "$expected" =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] \
  || { printf 'QOS_LINUX_EXPECTED_FINGERPRINT must be a full OpenPGP fingerprint\n' >&2; exit 1; }
[ -s "$artifact" ] || { printf 'release payload is missing: %s\n' "$artifact" >&2; exit 1; }
[ ! -e "$signature" ] || { printf 'refusing to replace release signature: %s\n' "$signature" >&2; exit 1; }

selector="${QOS_LINUX_VERIFY_KEY:-${QOS_LINUX_SIGNING_KEY:-}}"
[ -n "$selector" ] || { printf 'no Linux release-key selector was configured\n' >&2; exit 1; }
mapfile -t fingerprints < <(
  gpg --batch --with-colons --fingerprint "$selector" 2>/dev/null \
    | awk -F: '$1 == "pub" { want = 1; next }
                 want && $1 == "fpr" { print toupper($10); want = 0 }'
)
[ "${#fingerprints[@]}" -eq 1 ] && [ "${fingerprints[0]}" = "$expected" ] \
  || { printf 'Linux release key does not match protected release policy\n' >&2; exit 1; }

if [ -n "${QOS_LINUX_SIGNER:-}" ]; then
  "$QOS_LINUX_SIGNER" "$artifact" "$signature"
elif [ -n "${QOS_LINUX_SIGNING_KEY:-}" ]; then
  gpg --batch --yes --local-user "$QOS_LINUX_SIGNING_KEY" \
    --armor --detach-sign --output "$signature" "$artifact"
else
  printf 'Linux signing credentials are required\n' >&2
  exit 1
fi
[ -s "$signature" ] || { printf 'Linux signer did not create %s\n' "$signature" >&2; exit 1; }

identity="$(gpg --batch --status-fd 1 --verify "$signature" "$artifact" 2>/dev/null \
  | awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" { print $3 " " $NF; exit }')"
read -r signing_fingerprint primary_fingerprint <<<"$identity"
if [ "$signing_fingerprint" != "$expected" ] \
   && [ "$primary_fingerprint" != "$expected" ]; then
  printf 'signature for %s was not made by expected release key %s\n' \
    "$(basename "$artifact")" "$expected" >&2
  exit 1
fi
