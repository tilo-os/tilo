#!/bin/sh
# Signing preflight: fail unless the configured KMS/HSM signing key is exactly
# the public key pinned in keys/cosign.pub. Publishing jobs depend on this, so
# no native build starts under a key that verifiers would reject.
set -eu
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
case ${COSIGN_KEY_REF:-} in
  gcpkms://*|awskms://*|azurekms://*|hashivault://*) ;;
  *) echo '::error::COSIGN_KEY_REF must name a non-exportable KMS/HSM key' >&2; exit 1 ;;
esac
. "$repo/os/dev/configure-cosign-kms.sh"
resolved=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/signing-authority.XXXXXX")
cosign public-key --key "$COSIGN_KEY_REF" > "$resolved"
if ! cmp --silent "$repo/keys/cosign.pub" "$resolved"; then
  rm -f -- "$resolved"
  echo '::error::KMS/HSM signing authority does not match keys/cosign.pub' >&2
  exit 1
fi
rm -f -- "$resolved"
echo 'KMS/HSM signing authority matches keys/cosign.pub'
