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
if [[ -f $walks/$walk.json ]]; then
    # A walk's check is the family it names, else the walk's own.
    check=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("family") or sys.argv[2])' \
        "$walks/$walk.json" "$walk")
else
    # A family: the check that walks every walk naming it, in their order.
    python3 -c 'import json,pathlib,sys
name, walks = sys.argv[2], pathlib.Path(sys.argv[1])
if not any(json.loads(path.read_text()).get("family") == name
           for path in walks.glob("*.json")):
    raise SystemExit(1)' "$walks" "$walk" \
        || { echo "no walk or family named $walk (scripts/headless-examples/walk.sh --list)" >&2; exit 2; }
    check=$walk
fi

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
# The run's id is what --detach prints; its stream never names it.
run=$(t test -C "$repo_root" "app-walk/$check" ${inputs[@]+"${inputs[@]}"} --detach \
    | grep -oE '^r[0-9a-f]{10}' | head -1) || true
[[ -n $run ]] || { echo "t started no run for app-walk/$check" >&2; exit 1; }
echo "run $run: app-walk/$check" >&2
# Followed in pieces: one follow ends after ten minutes, a walk behind a session build takes longer.
while true; do
    code=0
    timeout 590 t follow -C "$repo_root" "$run" > "$out/run.log" 2>&1 || code=$?
    [[ $code -eq 124 ]] || break
done
[[ $code -eq 0 ]] || status=$code
t outputs -C "$repo_root" "app-walk/$check" --run "$run" --to "$out/now" > /dev/null
now=$(find "$out/now" -path "*/$walk/walk.json" -printf '%h\n' | head -1)
[[ -n $now ]] || now=$(find "$out/now" -name walk.json -printf '%h\n' | head -1)

if ((write_baseline)); then
    # The baseline is what main walked, with the run that keeps its shots; each step's time is
    # the slowest main has shown for it (app-walk.py's rebaseline).
    python3 - "$now/summary.json" "$walks/baseline/$walk.json" "$run" "$repo_root/scripts/headless-examples/app-walk.py" <<'EOF'
import importlib.util, json, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("app_walk", sys.argv[4])
walk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(walk)
found = json.load(open(sys.argv[1]))
failed = walk.unwalked(found)
if failed:
    # A baseline that held a failure would turn it green on main, hiding its fix or its
    # getting worse; the walk stays red until it passes.
    for step in failed:
        print(f"not a baseline: {step['version']} {step['step']} failed: {step['why']}",
              file=sys.stderr)
    sys.exit(1)
found["run"] = sys.argv[3]
kept = Path(sys.argv[2])
before = json.loads(kept.read_text()) if kept.is_file() else None
kept.write_text(json.dumps(walk.rebaseline(found, before), indent=1) + "\n")
EOF
    echo "baseline written: $walks/baseline/$walk.json (run $run)"
    exit "$status"
fi

baseline=""
base_run=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("run") or "")' \
    "$walks/baseline/$walk.json" 2>/dev/null || true)
if [[ -n $base_run ]]; then
    if [[ ! -d $out/baseline-$base_run ]]; then
        t outputs -C "$repo_root" "app-walk/$check" --run "$base_run" --to "$out/baseline-$base_run" \
            > /dev/null || echo "the baseline run $base_run kept no outputs" >&2
    fi
    baseline=$(find "$out/baseline-$base_run" -path "*/$walk/walk.json" -printf '%h\n' 2>/dev/null | head -1)
    [[ -n $baseline ]] || baseline=$(find "$out/baseline-$base_run" -name walk.json -printf '%h\n' 2>/dev/null | head -1)
fi
python3 "$repo_root/scripts/headless-examples/app-walk.py" --side-by-side "$now" \
    ${baseline:+--baseline-dir "$baseline"} --out "$out"
echo "$out/index.md"
exit "$status"
