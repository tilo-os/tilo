#!/usr/bin/env bash
# Compare the scheduled performance run's records (perf-scheduled.sh writes
# them to /target/perf/<group>/) with the store, and on main publish them.
#
# T_INPUT_ACCEPT names KPI ids (or all) whose values start a new baseline, on
# main only; T_INPUT_SYNTHETIC_SERIES and T_INPUT_SYNTHETIC_FACTOR tag the
# records as a synthetic series for a gate test, which is published from any
# reference and never enters a real baseline. TILO_PACKAGE_TOKEN (or t's
# TILO_PACKAGES_TOKEN, or REGISTRY_TOKEN) writes the store.
set -uo pipefail
shopt -s nullglob

perf=${CARGO_TARGET_DIR:-/target}/perf
out=$perf/compare
store=${TILO_PERF_STORE:-https://code.tilo.so}
TILO_PACKAGE_TOKEN=${TILO_PACKAGE_TOKEN:-${TILO_PACKAGES_TOKEN:-${REGISTRY_TOKEN:-}}}
export TILO_PACKAGE_TOKEN
owner=tilo
rm -rf "$out"
mkdir -p "$out"

results=("$perf"/*/*.jsonl)
groups=()
for file in "${results[@]}"; do
  group=$(basename "$(dirname "$file")")
  groups+=("${group%%-*}")  # hosting-room is hosting's, footprint-image footprint's
done
if (( ${#results[@]} == 0 )); then
  echo "no benchmark produced records" >&2
  exit 1
fi

on_main=false
[[ ${T_REF:-} == main ]] && on_main=true
accept=${T_INPUT_ACCEPT:-}
series=${T_INPUT_SYNTHETIC_SERIES:-}
if [[ -n $accept && $on_main != true ]]; then
  echo "accept only applies to main, where the store is written" >&2
  exit 1
fi
args=(--store "$store" --owner "$owner" --lane perf-scheduled
      --groups "$(printf '%s\n' "${groups[@]}" | sort -u | paste -sd, -)"
      --publish-dir "$out/publish" --run "t-${T_COMMIT:-tree}-$(date -u +%Y%m%dT%H%M%SZ)"
      --summary "$out/summary.md")
[[ -n $accept ]] && args+=(--accept "$accept")
[[ -n $series ]] && args+=(--synthetic "$series" --synthetic-factor "${T_INPUT_SYNTHETIC_FACTOR:-1.0}")
status=0
python3 tools/perf/compare.py --results "${results[@]}" "${args[@]}" || status=1
if [[ $on_main == true || -n $series ]] && [[ -n ${TILO_PACKAGE_TOKEN:-} ]]; then
  manifest=$(mktemp)
  for file in "$out"/publish/*/*.jsonl; do
    month=$(basename "$(dirname "$file")")
    printf '%s\t%s\n' "$file" "$store/api/packages/$owner/generic/tilo-perf-results/$month/$(basename "$file")"
  done > "$manifest"
  bash tools/upload_immutable_packages.sh "$manifest" || status=1
else
  echo "Not main and not synthetic, or no store token: compared only, nothing written to the store."
fi
[[ -f $out/summary.md ]] && cat "$out/summary.md"
exit "$status"
