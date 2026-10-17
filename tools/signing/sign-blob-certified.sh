#!/usr/bin/env bash
# Sign FILE with an operational key and enter the signature in the public
# transparency log (Rekor) under the key's operational certificate.
#
#   sign-blob-certified.sh PURPOSE CERT FILE OUT [EPOCH]
#
# writes OUT.sig (base64, as `cosign sign-blob` writes it) and OUT.bundle (the
# log entry in Cosign's bundle form, which `cosign attach signature
# --rekor-response` and `cosign verify-blob --bundle` take), then verifies both
# as an installed system would: CERT must chain to keys/release/root.crt for
# PURPOSE and EPOCH (default: the current one; an older one for a bridge
# certificate, see keys/README.md), and be valid when the log recorded the entry.
#
# `cosign sign-blob` logs the bare public key and has no way to name a
# certificate, but a certificate-anchored verifier (containers-policy's
# `fulcio`, cosign --certificate-chain) requires the log entry to hold the
# certificate. So Cosign only signs (no log upload) and the entry is made here
# through Rekor's API: a hashedrekord of FILE's SHA-256, the signature and CERT.
#
# Environment: COSIGN_KEY_REF and whatever os/dev/configure-cosign-kms.sh needs,
# already sourced by the caller; TILO_REKOR_URL (default the public instance);
# TILO_RELEASE_ROOT (default keys/release/root.crt).
# Runs with bash, curl, sed, base64 and sha256sum only: t's signing checks run
# in the skopeo image, which has no Python or jq.
set -euo pipefail

purpose=${1:?usage: sign-blob-certified.sh PURPOSE CERT FILE OUT}
cert=${2:?}
file=${3:?}
out=${4:?}
epoch=${5:-}
repo=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
rekor=${TILO_REKOR_URL:-https://rekor.sigstore.dev}
root=${TILO_RELEASE_ROOT:-$repo/keys/release/root.crt}

die() { printf 'sign-blob-certified: %s\n' "$*" >&2; exit 1; }
[[ -s $cert ]] || die "no operational certificate at $cert"
[[ -s $file ]] || die "nothing to sign at $file"
: "${COSIGN_KEY_REF:?COSIGN_KEY_REF is required}"

cosign sign-blob --yes --tlog-upload=false --key "$COSIGN_KEY_REF" \
  --output-signature "$out.sig" "$file" >/dev/null
signature=$(tr -d '\n' < "$out.sig")
digest=$(sha256sum "$file" | cut -d' ' -f1)
certificate=$(base64 -w0 < "$cert")
request=$(printf '{"apiVersion":"0.0.1","kind":"hashedrekord","spec":{"signature":{"content":"%s","publicKey":{"content":"%s"}},"data":{"hash":{"algorithm":"sha256","value":"%s"}}}}' \
  "$signature" "$certificate" "$digest")
response=$(curl --fail-with-body --silent --show-error --retry 3 \
  --header 'Content-Type: application/json' --data "$request" "$rekor/api/v1/log/entries") \
  || die "Rekor refused the entry: $response"

# The response is one JSON object keyed by the entry's UUID. Each value read
# here is base64, hex or decimal, so a pattern over those alphabets cannot run
# into a neighbouring field. The entry's logIndex is the one right after its
# logID: the inclusion proof has a logIndex of its own, counted within its
# shard. A misread value fails the verification below, never passes it.
body=$(printf '%s' "$response" | sed -n 's/.*"body":"\([A-Za-z0-9+/=]*\)".*/\1/p')
integrated=$(printf '%s' "$response" | sed -n 's/.*"integratedTime":\([0-9]*\).*/\1/p')
log_id=$(printf '%s' "$response" | sed -n 's/.*"logID":"\([0-9a-f]*\)".*/\1/p')
log_index=$(printf '%s' "$response" | sed -n 's/^[^"]*"[0-9a-f]*":{"body":"[^"]*","integratedTime":[0-9]*,"logID":"[0-9a-f]*","logIndex":\([0-9]*\).*/\1/p')
set_signature=$(printf '%s' "$response" | sed -n 's/.*"signedEntryTimestamp":"\([A-Za-z0-9+/=]*\)".*/\1/p')
[[ -n $body && -n $integrated && -n $log_id && -n $log_index && -n $set_signature ]] \
  || die "unexpected Rekor response: $response"
# Cosign's own bundle form (what `cosign sign-blob --bundle` writes): the
# signature, the certificate and the log entry with its signed timestamp.
printf '{"base64Signature":"%s","cert":"%s","rekorBundle":{"SignedEntryTimestamp":"%s","Payload":{"body":"%s","integratedTime":%s,"logIndex":%s,"logID":"%s"}}}\n' \
  "$signature" "$certificate" "$set_signature" "$body" "$integrated" "$log_index" "$log_id" > "$out.bundle"

# shellcheck source=keys/release/trust.env
. "$repo/keys/release/trust.env"
cosign verify-blob --certificate "$cert" --certificate-chain "$root" \
  --certificate-identity "$purpose@$TILO_RELEASE_IDENTITY_DOMAIN" \
  --certificate-oidc-issuer "$TILO_RELEASE_ISSUER_PREFIX${epoch:-$TILO_RELEASE_EPOCH}" \
  --insecure-ignore-sct --bundle "$out.bundle" --signature "$out.sig" "$file" >/dev/null 2>&1 \
  || die "the certified signature of $file does not verify for $purpose; is $cert current and issued for this key?"
printf 'logged %s as entry %s under %s\n' "$file" "$log_index" "$cert"
