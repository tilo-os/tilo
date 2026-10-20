#!/usr/bin/env bash
# Sign one Windows executable with Authenticode in place, timestamp it, and
# check the signer: jsign signs with a key that never leaves AWS KMS (it asks
# KMS to sign the digest) and embeds the certificate chain it is given. The
# signature itself is verified where Windows checks it (docs/publishing-on-t.md,
# Windows); this image has no OpenSSL to do it with.
#
#   authenticode.sh <file.exe>
#
# Environment (t.toml, t's secrets and sign.sh; docs/publishing-on-t.md, Windows):
#   TILO_AUTHENTICODE_KMS_KEY     arn:aws:kms:<region>:<account>:alias/<name> (or key ARN)
#   TILO_AUTHENTICODE_CERT_CHAIN  the code-signing certificate and its intermediates, PEM
#   TILO_WINDOWS_PUBLISHER        the certificate subject every signature must carry,
#                                 as `keytool -printcert` shows it (CN=…, O=…, C=…)
#   AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN
#                                 a role session that may kms:Sign with that key
# TILO_AUTHENTICODE_TIMESTAMP_URL overrides the RFC 3161 timestamp service.
set -euo pipefail

file="${1:?executable to sign}"
: "${TILO_AUTHENTICODE_KMS_KEY:?the KMS key ARN to sign with}"
: "${TILO_AUTHENTICODE_CERT_CHAIN:?the PEM certificate chain of the signing key}"
: "${TILO_WINDOWS_PUBLISHER:?the expected certificate subject}"
: "${AWS_ACCESS_KEY_ID:?signing credentials}"
: "${AWS_SECRET_ACCESS_KEY:?signing credentials}"
timestamp="${TILO_AUTHENTICODE_TIMESTAMP_URL:-http://timestamp.digicert.com}"

[[ "$TILO_AUTHENTICODE_KMS_KEY" =~ ^arn:aws:kms:([a-z0-9-]+):[0-9]{12}:(alias|key)/.+$ ]] \
  || { printf 'TILO_AUTHENTICODE_KMS_KEY must be a KMS key or alias ARN\n' >&2; exit 2; }
region="${BASH_REMATCH[1]}"

work="$(mktemp -d "${TMPDIR:-/tmp}/authenticode.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
printf '%s\n' "$TILO_AUTHENTICODE_CERT_CHAIN" > "$work/chain.pem"

# The leaf certificate (the chain's first) must be the publisher's before KMS
# is asked to sign anything.
subject="$(keytool -printcert -file "$work/chain.pem" | sed -n 's/^Owner: //p' | head -n 1)"
if [ "$subject" != "$TILO_WINDOWS_PUBLISHER" ]; then
  printf 'certificate subject %s is not the publisher %s\n' "$subject" "$TILO_WINDOWS_PUBLISHER" >&2
  exit 1
fi

# jsign reads the AWS credentials, session token included, from the environment
# when no --storepass is given, so they never appear on a command line.
AWS_REGION="$region" \
  jsign --storetype AWS --keystore "$region" --alias "$TILO_AUTHENTICODE_KMS_KEY" \
    --certfile "$work/chain.pem" --alg SHA-256 \
    --tsaurl "$timestamp" --tsmode RFC3161 --tsretries 5 --tsretrywait 10 \
    --replace "$file"

printf 'signed %s as %s\n' "$(basename -- "$file")" "$TILO_WINDOWS_PUBLISHER"
