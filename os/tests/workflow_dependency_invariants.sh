#!/usr/bin/env bash
# Every remote workflow action is executable third-party code. Require an exact
# commit rather than a mutable tag or branch across the entire workflow set.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflows="$repo/.forgejo/workflows"
fail=0

while IFS=: read -r file line match; do
  ref="${match#uses:}"
  ref="${ref#${ref%%[![:space:]]*}}"
  case "$ref" in
    ./*|docker://*) continue ;;
  esac
  revision="${ref##*@}"
  if [[ ! "$revision" =~ ^[0-9a-fA-F]{40}$ ]]; then
    printf 'mutable workflow action: %s:%s: %s\n' "$file" "$line" "$ref" >&2
    fail=1
  fi
done < <(grep -R -n -o -E 'uses:[[:space:]]*[^[:space:]#]+' "$workflows")

if [ "$fail" = 0 ]; then
  printf 'ok    every remote workflow action is pinned to a full commit SHA\n'
fi
exit "$fail"
