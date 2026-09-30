#!/usr/bin/env bash
# Verify the complete registry SBOM against its digest in Rekor-logged SLSA
# provenance. Rekor's public 100 KB attestation cap excludes full OS SPDX data.
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: $0 IMAGE@sha256:DIGEST [COSIGN_PUBLIC_KEY]" >&2
  exit 2
fi
image=$1
key=${2:-keys/cosign.pub}
digest=${image##*@}
[[ "$image" == *@sha256:* && "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] || {
  echo "release SBOM verification requires a digest-pinned image" >&2
  exit 1
}

# cosign verifies the KMS signature and Rekor inclusion before emitting the
# envelope. Multiple identical attestations can exist after a CI retry; a
# conflicting digest is never accepted.
provenance="$(cosign verify-attestation --key "$key" --type slsaprovenance1 "$image")"
expected="$(printf '%s' "$provenance" | jq -er --arg digest "$digest" '
  [(if type == "array" then .[] else . end)
    | .payload | @base64d | fromjson | .predicate
    | select(.buildDefinition.internalParameters.expectedReleaseDigest == $digest)
    | .runDetails.byproducts[]?
    | select(.name == "spdx-sbom") | .uri
    | select(type == "string" and test("^sha256:[0-9a-f]{64}$"))]
  | unique | if length == 1 then .[0] else error("missing or conflicting SBOM digest") end
')"
actual="$(cosign download sbom "$image" | sha256sum | cut -d' ' -f1)"
if [ "$expected" != "sha256:$actual" ]; then
  echo "release SBOM digest mismatch for $image" >&2
  exit 1
fi
printf 'verified SPDX SBOM sha256:%s for %s\n' "$actual" "$image"
