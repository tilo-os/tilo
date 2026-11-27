#!/usr/bin/env bash
# Drop from a registry entry the versions whose package the store no longer serves.
#
#   drop-unserved-versions.sh <entry.json>
#
# A new request builds on the one still waiting for review, so a version whose
# package URL the store refuses (404, 403 or 410) would ride along in every
# later request, and the collector, which fetches every version it is given,
# would keep the whole request pending. Only a definite refusal drops a
# version: one that fails to answer at all is kept, since its package may well
# be there. Rewrites the file in place; an empty file is left alone.
set -euo pipefail
entry=$1
[[ -s $entry ]] || exit 0
refused=()
while IFS= read -r url; do
  status=$(curl --silent --output /dev/null --head --location --connect-timeout 10 \
    --max-time 30 --retry 3 --write-out '%{http_code}' "$url" || true)
  case $status in
    403|404|410)
      printf 'dropping %s from the request: the store answers HTTP %s for it\n' "$url" "$status" >&2
      refused+=("$url")
      ;;
  esac
done < <(jq -r '.versions[] | .tapp_url // empty' "$entry")
(( ${#refused[@]} )) || exit 0
jq --args '.versions |= map(select((.tapp_url // "") as $url | $ARGS.positional | index($url) | not))' \
  "${refused[@]}" < "$entry" > "$entry.served"
mv -f -- "$entry.served" "$entry"
