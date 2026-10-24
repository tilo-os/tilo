#!/usr/bin/env bash
# Emit the canonical short-lived freshness predicate attached to a Tilo release.
# This is signed as an in-toto attestation. Independent witnesses can reproduce
# and sign the same schema without sharing the release-signing key.
set -euo pipefail

out="${1:-freshness.json}"
: "${RELEASE_DIGEST:?RELEASE_DIGEST is required}"
: "${TILO_GENERATION:?TILO_GENERATION is required}"
: "${SOURCE_REVISION:?SOURCE_REVISION is required}"
# Board images are published to the forge's private registry; every other OS
# image to the public one.
repository="${TILO_OS_REPOSITORY:-registry.tilo.so/tilo/tilo}"
case "$repository" in
  registry.tilo.so/tilo/tilo|code.tilo.so/tilo/tilo) ;;
  *) echo 'release freshness: unknown OS repository' >&2; exit 1 ;;
esac

case "$RELEASE_DIGEST" in sha256:[0-9a-f][0-9a-f]*) ;; *) echo 'release freshness: invalid digest' >&2; exit 1 ;; esac
digest_hex="${RELEASE_DIGEST#sha256:}"
[ "${#digest_hex}" -eq 64 ] && case "$digest_hex" in *[!0-9a-f]*) false ;; *) true ;; esac \
  || { echo 'release freshness: digest must be sha256 plus 64 lowercase hex characters' >&2; exit 1; }
case "$TILO_GENERATION" in ''|*[!0-9]*) echo 'release freshness: generation must be an integer' >&2; exit 1 ;; esac
[ "$TILO_GENERATION" -gt 0 ] || { echo 'release freshness: generation must be positive' >&2; exit 1; }
case "$SOURCE_REVISION" in *[!0-9a-f]*|'') echo 'release freshness: source revision must be lowercase hex' >&2; exit 1 ;; esac
[ "${#SOURCE_REVISION}" -eq 40 ] || { echo 'release freshness: source revision must be 40 hex characters' >&2; exit 1; }

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

# The keys in order, as `jq -S` writes them; every value is checked above to need no escaping.
printf '{"digest":"%s","expiresAt":%s,"generation":%s,"issuedAt":%s,"repository":"%s","schema":"https://tilo.sh/security/release-freshness/v1","sourceRevision":"%s"}\n' \
  "$RELEASE_DIGEST" "$expires" "$TILO_GENERATION" "$issued" "$repository" "$SOURCE_REVISION" > "$out"
