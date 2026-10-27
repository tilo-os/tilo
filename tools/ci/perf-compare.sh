#!/usr/bin/env bash
# Compare the performance run's records (perf-scheduled.sh writes them to
# /target/perf/<group>/) with the store, and publish them from a trusted run
# (T_REF is set: a run t started for a branch, or a dispatch) that holds the
# store's token. Which references get the token is the server's secret config,
# so that decides where results are recorded: main and stable, say.
#
# T_INPUT_ACCEPT names KPI ids (or all) whose values start a new baseline,
# where the store is written; T_INPUT_SYNTHETIC_SERIES and
# T_INPUT_SYNTHETIC_FACTOR tag the records as a synthetic series for a gate
# test, which never enters a real baseline. TILO_PACKAGE_TOKEN (or t's
# TILO_PACKAGES_TOKEN, or REGISTRY_TOKEN) writes the store.
#
# perf-scheduled runs this whatever its groups' outcome (needs_any_outcome), so
# the records of the groups that passed are still compared and published.
# T_NEEDS_FAILED names the groups that did not pass; they fail the run, and so
# does every group of the lane that left no records.
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

status=0
for failed in ${T_NEEDS_FAILED:-}; do
  echo "$failed did not pass: its group is compared only for the records it left" >&2
  status=1
done
# hosting-room's records are hosting's and footprint-image's footprint's, so the
# lane's groups are what every run owes.
groups=$(python3 -c 'import sys, tomllib
print(",".join(tomllib.load(open(sys.argv[1], "rb"))["lanes"]["perf-scheduled"]["groups"]))' \
  tools/perf/kpi-budgets.toml) || exit 1
results=("$perf"/*/*.jsonl)
if (( ${#results[@]} == 0 )); then
  echo "no benchmark produced records" >&2
  exit 1
fi

writes=false
[[ -n ${T_REF:-} && -n ${TILO_PACKAGE_TOKEN:-} ]] && writes=true
accept=${T_INPUT_ACCEPT:-}
series=${T_INPUT_SYNTHETIC_SERIES:-}
if [[ -n $accept && $writes != true ]]; then
  echo "accept applies only where the store is written: a trusted run holding its token" >&2
  exit 1
fi
args=(--store "$store" --owner "$owner" --lane perf-scheduled --groups "$groups"
      --publish-dir "$out/publish" --run "t-${T_COMMIT:-tree}-$(date -u +%Y%m%dT%H%M%SZ)"
      --summary "$out/summary.md")
[[ -n $accept ]] && args+=(--accept "$accept")
[[ -n $series ]] && args+=(--synthetic "$series" --synthetic-factor "${T_INPUT_SYNTHETIC_FACTOR:-1.0}")
python3 tools/perf/compare.py --results "${results[@]}" "${args[@]}" || status=1
if [[ $writes == true || ( -n $series && -n ${TILO_PACKAGE_TOKEN:-} ) ]]; then
  manifest=$(mktemp)
  for file in "$out"/publish/*/*.jsonl; do
    month=$(basename "$(dirname "$file")")
    printf '%s\t%s\n' "$file" "$store/api/packages/$owner/generic/tilo-perf-results/$month/$(basename "$file")"
  done > "$manifest"
  bash tools/upload_immutable_packages.sh "$manifest" || status=1
else
  echo "Not a trusted run holding the store's token: compared only, nothing written to the store."
fi
[[ -f $out/summary.md ]] && cat "$out/summary.md"
exit "$status"
