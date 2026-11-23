#!/usr/bin/env bash
# Walk one part of a person's first hour on t and put this tree's run beside main's baseline.
#
#   scripts/headless-examples/walk.sh WALK [--only rust|tl] [--discover]   this tree, held to the baseline
#   scripts/headless-examples/walk.sh --list                                the walks there are
#   scripts/headless-examples/walk.sh WALK --write-baseline                 (on main) make this run the baseline
#
# The walk is the t check `app-walk/WALK` (t.toml): its step file is
# scripts/headless-examples/app-walks/WALK.json, its baseline app-walks/baseline/WALK.json. The
# check is red when a step main walked is not walked here, or is walked twice as slowly; this
# script runs it, brings back its outputs to target/walk/WALK/now, the baseline run's outputs to
# target/walk/WALK/baseline, and writes target/walk/WALK/index.md: every step, the baseline's shots
# beside this tree's, and what the comparison found. The exit status is the check's.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
walks=$repo_root/scripts/headless-examples/app-walks

if [[ ${1:-} == --list || -z ${1:-} ]]; then
    find "$walks" -maxdepth 1 -name '*.json' -printf '%f\n' | sed 's/\.json$//' | sort
    exit 0
fi
walk=$1
shift
[[ -f $walks/$walk.json ]] || { echo "no walk named $walk (scripts/headless-examples/walk.sh --list)" >&2; exit 2; }

inputs=()
write_baseline=0
while (($#)); do
    case $1 in
        --only) inputs+=(--input "only=$2"); shift 2 ;;
        --discover) inputs+=(--input discover=1); shift ;;
        --write-baseline) write_baseline=1; shift ;;
        *) echo "unknown argument $1" >&2; exit 2 ;;
    esac
done

out=$repo_root/target/walk/$walk
rm -rf -- "$out/now"
mkdir -p -- "$out"

status=0
t test -C "$repo_root" "app-walk/$walk" ${inputs[@]+"${inputs[@]}"} --json > "$out/run.json" || status=$?
run=$(python3 -c 'import json,sys
for line in open(sys.argv[1]):
    line = line.strip()
    if not line.startswith("{"): continue
    try: doc = json.loads(line)
    except ValueError: continue
    run = doc.get("run") or doc.get("run_id") or (doc.get("result") or {}).get("run")
    if run: print(run); break' "$out/run.json")
[[ -n $run ]] || { echo "t named no run (see $out/run.json)" >&2; exit 1; }
t outputs -C "$repo_root" "app-walk/$walk" --run "$run" --to "$out/now" > /dev/null
now=$(find "$out/now" -name walk.json -printf '%h\n' | head -1)

if ((write_baseline)); then
    # The baseline is what main walked, with the run that keeps its shots.
    python3 - "$now/summary.json" "$walks/baseline/$walk.json" "$run" <<'EOF'
import json, sys
found = json.load(open(sys.argv[1]))
found["run"] = sys.argv[3]
json.dump(found, open(sys.argv[2], "w"), indent=1)
open(sys.argv[2], "a").write("\n")
EOF
    echo "baseline written: $walks/baseline/$walk.json (run $run)"
    exit "$status"
fi

baseline=""
base_run=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("run") or "")' \
    "$walks/baseline/$walk.json" 2>/dev/null || true)
if [[ -n $base_run ]]; then
    if [[ ! -d $out/baseline-$base_run ]]; then
        t outputs -C "$repo_root" "app-walk/$walk" --run "$base_run" --to "$out/baseline-$base_run" \
            > /dev/null || echo "the baseline run $base_run kept no outputs" >&2
    fi
    baseline=$(find "$out/baseline-$base_run" -name walk.json -printf '%h\n' 2>/dev/null | head -1)
fi
python3 "$repo_root/scripts/headless-examples/app-walk.py" --side-by-side "$now" \
    ${baseline:+--baseline-dir "$baseline"} --out "$out"
echo "$out/index.md"
exit "$status"
