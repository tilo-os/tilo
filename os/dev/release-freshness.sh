#!/usr/bin/env bash
# Emit the canonical short-lived freshness predicate attached to a qOS release.
# This is signed as an in-toto attestation. Independent witnesses can reproduce
# and sign the same schema without sharing the release-signing key.
set -euo pipefail

out="${1:-freshness.json}"
: "${RELEASE_DIGEST:?RELEASE_DIGEST is required}"
: "${QOS_GENERATION:?QOS_GENERATION is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"

case "$RELEASE_DIGEST" in sha256:[0-9a-f][0-9a-f]*) ;; *) echo 'release freshness: invalid digest' >&2; exit 1 ;; esac
digest_hex="${RELEASE_DIGEST#sha256:}"
[ "${#digest_hex}" -eq 64 ] && case "$digest_hex" in *[!0-9a-f]*) false ;; *) true ;; esac \
  || { echo 'release freshness: digest must be sha256 plus 64 lowercase hex characters' >&2; exit 1; }
case "$QOS_GENERATION" in ''|*[!0-9]*) echo 'release freshness: generation must be an integer' >&2; exit 1 ;; esac
[ "$QOS_GENERATION" -gt 0 ] || { echo 'release freshness: generation must be positive' >&2; exit 1; }
case "$GITHUB_SHA" in *[!0-9a-f]*|'') echo 'release freshness: source revision must be lowercase hex' >&2; exit 1 ;; esac
[ "${#GITHUB_SHA}" -eq 40 ] || { echo 'release freshness: source revision must be 40 hex characters' >&2; exit 1; }

issued="${FRESHNESS_ISSUED_AT:-$(date -u +%s)}"
ttl="${FRESHNESS_TTL_SECONDS:-86400}"
case "$issued:$ttl" in *[!0-9:]*) echo 'release freshness: timestamps must be integers' >&2; exit 1 ;; esac
[ "$issued" -gt 0 ] || { echo 'release freshness: issued-at must be positive' >&2; exit 1; }
[ "$ttl" -ge 300 ] && [ "$ttl" -le 604800 ] || {
  echo 'release freshness: lifetime must be between 5 minutes and 7 days' >&2
  exit 1
}
expires=$(( issued + ttl ))
[ "$expires" -gt "$issued" ] || { echo 'release freshness: timestamp overflow' >&2; exit 1; }

jq -S -n \
  --arg repository 'code.sandbox.example/qos/qos' \
  --arg digest "$RELEASE_DIGEST" \
  --arg source "$GITHUB_SHA" \
  --argjson generation "$QOS_GENERATION" \
  --argjson issued "$issued" \
  --argjson expires "$expires" \
  '{
    schema: "https://qos.dev/security/release-freshness/v1",
    repository: $repository,
    digest: $digest,
    generation: $generation,
    sourceRevision: $source,
    issuedAt: $issued,
    expiresAt: $expires
  }' > "$out"
