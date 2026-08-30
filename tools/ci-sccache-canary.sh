#!/usr/bin/env bash
# Record sccache counters and require a remote hit in the warm canary job.
set -euo pipefail

mode="${1:?usage: ci-sccache-canary.sh <cold|warm>}"
case "$mode" in cold|warm) ;; *) echo "unknown mode: $mode" >&2; exit 2;; esac

stats="$(sccache --show-stats --stats-format=json)"
printf '%s\n' "$stats"
hits="$({ printf '%s' "$stats" | python3 -c '
import json, sys
counts = json.load(sys.stdin).get("stats", {}).get("cache_hits", {}).get("counts", {})
print(sum(counts.values()))
'; })"

echo "sccache_${mode}_hits=${hits}"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  printf '### sccache %s canary\n\n- cache hits: %s\n' "$mode" "$hits" \
    >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$mode" = warm ] && [ "$hits" -le 0 ]; then
  echo 'warm canary produced no cache hits from the cold job' >&2
  exit 1
fi
