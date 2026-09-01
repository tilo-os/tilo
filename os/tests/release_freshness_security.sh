#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

digest="sha256:$(printf 'a%.0s' {1..64})"
source="$(printf 'b%.0s' {1..40})"
env RELEASE_DIGEST="$digest" QOS_GENERATION=42 GITHUB_SHA="$source" \
  FRESHNESS_ISSUED_AT=2000000000 FRESHNESS_TTL_SECONDS=3600 \
  "$repo/os/dev/release-freshness.sh" "$tmp/one.json"
env RELEASE_DIGEST="$digest" QOS_GENERATION=42 GITHUB_SHA="$source" \
  FRESHNESS_ISSUED_AT=2000000000 FRESHNESS_TTL_SECONDS=3600 \
  "$repo/os/dev/release-freshness.sh" "$tmp/two.json"
cmp "$tmp/one.json" "$tmp/two.json"
jq -e --arg digest "$digest" --arg source "$source" '
  .schema == "https://qos.dev/security/release-freshness/v1"
  and .repository == "code.podesta.ai/qos/qos"
  and .digest == $digest
  and .sourceRevision == $source
  and .generation == 42
  and .issuedAt == 2000000000
  and .expiresAt == 2000003600
' "$tmp/one.json" >/dev/null

if env RELEASE_DIGEST="$digest" QOS_GENERATION=42 GITHUB_SHA="$source" \
  FRESHNESS_ISSUED_AT=2000000000 FRESHNESS_TTL_SECONDS=604801 \
  "$repo/os/dev/release-freshness.sh" "$tmp/too-long.json" 2>/dev/null; then
  echo 'freshness generator accepted a lifetime over seven days' >&2
  exit 1
fi
if env RELEASE_DIGEST='sha256:not-a-digest' QOS_GENERATION=42 GITHUB_SHA="$source" \
  "$repo/os/dev/release-freshness.sh" "$tmp/bad.json" 2>/dev/null; then
  echo 'freshness generator accepted an invalid digest' >&2
  exit 1
fi

workflow="$repo/.forgejo/workflows/build-image.yml"
grep -Fq -- '--type https://qos.dev/security/release-freshness/v1' "$workflow"
grep -Fq 'release freshness attestation is absent, expired, or does not bind this release' "$workflow"
grep -Fq 'and .expiresAt > $now' "$workflow"

printf 'ok    release freshness is short-lived, signed and promotion-blocking\n'
