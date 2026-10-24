#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

digest="sha256:$(printf 'a%.0s' {1..64})"
source="$(printf 'b%.0s' {1..40})"
env RELEASE_DIGEST="$digest" TILO_GENERATION=42 SOURCE_REVISION="$source" \
  FRESHNESS_ISSUED_AT=2000000000 FRESHNESS_TTL_SECONDS=3600 \
  "$repo/os/dev/release-freshness.sh" "$tmp/one.json"
env RELEASE_DIGEST="$digest" TILO_GENERATION=42 SOURCE_REVISION="$source" \
  FRESHNESS_ISSUED_AT=2000000000 FRESHNESS_TTL_SECONDS=3600 \
  "$repo/os/dev/release-freshness.sh" "$tmp/two.json"
cmp "$tmp/one.json" "$tmp/two.json"
jq -e --arg digest "$digest" --arg source "$source" '
  .schema == "https://tilo.sh/security/release-freshness/v1"
  and .repository == "registry.tilo.so/tilo/tilo"
  and .digest == $digest
  and .sourceRevision == $source
  and .generation == 42
  and .issuedAt == 2000000000
  and .expiresAt == 2000003600
' "$tmp/one.json" >/dev/null

if env RELEASE_DIGEST="$digest" TILO_GENERATION=42 SOURCE_REVISION="$source" \
  FRESHNESS_ISSUED_AT=2000000000 FRESHNESS_TTL_SECONDS=604801 \
  "$repo/os/dev/release-freshness.sh" "$tmp/too-long.json" 2>/dev/null; then
  echo 'freshness generator accepted a lifetime over seven days' >&2
  exit 1
fi
if env RELEASE_DIGEST='sha256:not-a-digest' TILO_GENERATION=42 SOURCE_REVISION="$source" \
  "$repo/os/dev/release-freshness.sh" "$tmp/bad.json" 2>/dev/null; then
  echo 'freshness generator accepted an invalid digest' >&2
  exit 1
fi

release="$repo/tools/ci/publish/os-release.sh"
grep -Fq 'freshness.json https://tilo.sh/security/release-freshness/v1' "$release"
grep -Fq 'FRESHNESS_TTL_SECONDS=86400' "$release"

printf 'ok    release freshness is short-lived and signed with every release\n'
