#!/usr/bin/env bash
# Record sccache counters and require a remote hit in the warm canary job: of the runners'
# GHA cache (build-ci-builder.yml), or of t's compiler cache (tools/ci/publish/ci-builder-canary.sh).
set -euo pipefail

mode="${1:?usage: tools/ci/sccache-canary.sh <cold|warm>}"
case "$mode" in cold|warm) ;; *) echo "unknown mode: $mode" >&2; exit 2;; esac

expected_prefix="${TILO_SCCACHE_KEY_PREFIX:?canary requires TILO_SCCACHE_KEY_PREFIX}"
stats="$(sccache --show-stats --stats-format=json)"
printf '%s\n' "$stats"
parsed="$(printf '%s' "$stats" | python3 -c '
import json, os, sys

payload = json.load(sys.stdin)
stats = payload.get("stats", {})
location = payload.get("cache_location", "")
expected = os.environ["TILO_SCCACHE_KEY_PREFIX"]
# The GHA backend names its namespace "sccache-v<version>-<prefix>"; the WebDAV backend
# (the compiler cache t serves) says "webdav, name: , prefix: /<prefix>/".
if f"-{expected}," not in location + "," and f"prefix: /{expected}/" not in location:
    raise SystemExit(
        f"sccache used {location!r}, not requested namespace {expected!r}"
    )

def count(name):
    value = stats.get(name, {})
    if isinstance(value, dict):
        return sum(value.get("counts", {}).values())
    return int(value)

errors = count("cache_errors") + int(stats.get("cache_read_errors", 0)) + int(stats.get("cache_write_errors", 0))
print(count("cache_hits"), count("cache_misses"), int(stats.get("cache_writes", 0)), errors)
')"
read -r hits misses writes errors <<< "$parsed"

echo "sccache_${mode}_hits=${hits} misses=${misses} writes=${writes} errors=${errors} prefix=${expected_prefix}"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  printf '### sccache %s canary\n\n- prefix: `%s`\n- hits: %s\n- misses: %s\n- writes: %s\n- errors: %s\n' \
    "$mode" "$expected_prefix" "$hits" "$misses" "$writes" "$errors" \
    >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$errors" -ne 0 ]; then
  echo "$mode canary encountered remote cache errors" >&2
  exit 1
fi
if [ "$mode" = cold ]; then
  if [ "$hits" -ne 0 ]; then
    echo 'cold canary unexpectedly hit objects in its fresh run-attempt namespace' >&2
    exit 1
  fi
  if [ "$misses" -le 0 ] || [ "$writes" -le 0 ]; then
    echo 'cold canary neither missed nor populated its fresh namespace' >&2
    exit 1
  fi
fi
if [ "$mode" = warm ] && [ "$hits" -le 0 ]; then
  echo 'warm canary produced no cache hits from the cold job' >&2
  exit 1
fi
