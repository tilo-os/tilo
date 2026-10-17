#!/bin/sh
# Signing preflight: fail unless the configured KMS/HSM signing key is exactly
# the public key pinned in keys/cosign.pub and, once the release root exists,
# its operational certificate is current, chains to the root and certifies
# that key (tools/signing/release_trust.py). Publishing jobs depend on this, so
# no native build starts under a key or certificate that verifiers would reject.
set -eu
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
. "$repo/os/dev/configure-cosign-kms.sh"
resolved=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/signing-authority.XXXXXX")
cosign public-key --key "$COSIGN_KEY_REF" > "$resolved"
if ! cmp --silent "$repo/keys/cosign.pub" "$resolved"; then
  rm -f -- "$resolved"
  echo '::error::KMS/HSM signing authority does not match keys/cosign.pub' >&2
  exit 1
fi
if [ -e "$repo/keys/release/root.crt" ] \
   && ! python3 "$repo/tools/signing/release_trust.py" signing-certs os-images "$resolved" >/dev/null; then
  rm -f -- "$resolved"
  echo '::error::the operational certificate in keys/operational/ does not cover the KMS/HSM signing key' >&2
  exit 1
fi
rm -f -- "$resolved"
echo 'KMS/HSM signing authority matches keys/cosign.pub'
