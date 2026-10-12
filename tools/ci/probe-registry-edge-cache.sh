#!/usr/bin/env bash
# Check that the edge in front of a registry caches its immutable objects.
#
#   probe-registry-edge-cache.sh URL...
#
# GETs every URL twice (bodies discarded) and fails unless the second answer
# is 200, carries exactly one Cache-Control, that one is public and
# immutable, and Cloudflare served it from cache (cf-cache-status: HIT). Use
# a tapp, its .zst twin and a /blobs/<blake3> URL of apps.tilo.so; see
# docs/registry.md, "Edge cache for apps.tilo.so". A URL that cannot be
# fetched fails the probe: it observed nothing.
set -euo pipefail
(( $# > 0 )) || { printf 'usage: %s URL...\n' "${0##*/}" >&2; exit 2; }

failed=0
for url in "$@"; do
  headers=$(mktemp)
  for _ in 1 2; do
    : > "$headers"
    status=$(curl --silent --show-error --connect-timeout 10 --max-time 120 \
      --dump-header "$headers" --output /dev/null --write-out '%{http_code}' "$url") \
      || status=000
  done
  normalized=$(tr -d '\r' < "$headers" | tr '[:upper:]' '[:lower:]')
  rm -f "$headers"
  controls=$(grep -c '^cache-control:' <<< "$normalized" || true)
  control=$(sed -n 's/^cache-control: *//p' <<< "$normalized" | head -n 1)
  cache=$(sed -n 's/^cf-cache-status: *//p' <<< "$normalized" | head -n 1)
  problems=()
  [[ $status == 200 ]] || problems+=("HTTP $status")
  [[ $controls == 1 ]] || problems+=("$controls Cache-Control headers")
  [[ $control == *public* && $control == *immutable* ]] \
    || problems+=("Cache-Control '${control:-none}'")
  [[ $cache == hit ]] || problems+=("cf-cache-status '${cache:-none}' on the second fetch")
  if (( ${#problems[@]} )); then
    failed=1
    printf 'NOT CACHED %s: %s\n' "$url" "$(IFS=';'; printf '%s' "${problems[*]}")" >&2
  else
    printf 'cached %s\n' "$url"
  fi
done
exit "$failed"
