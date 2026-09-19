#!/usr/bin/env bash
# Run a CI command and publish wall-clock and process CPU usage. Collect numbers
# per selected shard so package weights can be tuned without another compilation
# pass.
set -euo pipefail

label="${1:?usage: tools/ci/measure.sh <label> -- <command...>}"
shift
[ "${1:-}" = "--" ] && shift
[ "$#" -gt 0 ] || { echo "tools/ci/measure.sh: no command given" >&2; exit 2; }

timing="$(mktemp -t ci-measure-XXXXXX)"
trap 'rm -f "$timing"' EXIT

status=0
if command -v /usr/bin/time >/dev/null 2>&1; then
  /usr/bin/time -f 'wall_seconds=%e\nuser_seconds=%U\nsystem_seconds=%S\nmax_rss_kib=%M' -o "$timing" "$@" || status=$?
else
  start="$(date +%s)"
  "$@" || status=$?
  printf 'wall_seconds=%s\nuser_seconds=unavailable\nsystem_seconds=unavailable\nmax_rss_kib=unavailable\n' \
    "$(( $(date +%s) - start ))" > "$timing"
fi

echo "==> ${label} measurements"
sed 's/^/    /' "$timing"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### ${label} resource use"
    echo '```text'
    cat "$timing"
    echo '```'
  } >> "$GITHUB_STEP_SUMMARY"
fi

exit "$status"
