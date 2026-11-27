#!/usr/bin/env bash
# Set aside a waiting request that names a package the store no longer serves.
#
#   set-aside-unserved-request.sh <entry.json>
#
# A new request builds on the one still waiting for review, so a version whose
# package URL the store refuses (404, 403 or 410) would ride along in every
# later request, and the collector, which fetches every version it is given,
# would keep the whole request pending. Such a request is not built on: the
# file is emptied, so the new request starts from the published entry, or
# afresh. Its publisher signed it whole, so no version can be taken out of it;
# a version it held for another architecture comes back with that
# architecture's next run. Only a definite refusal counts: a package that
# fails to answer at all may well be there. An empty file is left alone.
set -euo pipefail
entry=$1
[[ -s $entry ]] || exit 0
while IFS= read -r url; do
  status=$(curl --silent --output /dev/null --head --location --connect-timeout 10 \
    --max-time 30 --retry 3 --write-out '%{http_code}' "$url" || true)
  case $status in
    403|404|410)
      printf 'not building on the waiting request: the store answers HTTP %s for %s\n' \
        "$status" "$url" >&2
      : > "$entry"
      exit 0
      ;;
  esac
done < <(jq -r '.versions[] | .tapp_url // empty' "$entry")
