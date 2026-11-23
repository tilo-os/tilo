#!/usr/bin/env bash
# Run one of the target flows' hardware-only scenarios and hold it to what a
# scenario may report: every step passed, or a step ended naming a capability
# nobody has built yet. A genuine failure, a hang or an internal error is red.
#
#   flow_scenario.sh four-devices --arm-disk DISK [--server-disk DISK]
#   flow_scenario.sh laptop-phone-server [--server-disk DISK]
#
# The x86-64 desktop disk is os-dev-disk's disk.qcow2; TILO_FLOW_DESKTOP_DISK
# names it. Scenario steps whose capability is not built yet are the reason
# this check exists today: it proves they are named, not hung on.
set -euo pipefail

flow=${1:?usage: flow_scenario.sh four-devices|laptop-phone-server [disk flags]}
shift
case "$flow" in
four-devices | laptop-phone-server) ;;
*)
  echo "flow_scenario: unknown flow $flow (four-devices or laptop-phone-server)" >&2
  exit 2
  ;;
esac

vm=${TILO_VM_BIN:-tilo-vm}
out=${TILO_FLOW_OUT:-/target/flow-scenarios/$flow}
desktop=${TILO_FLOW_DESKTOP_DISK:-/target/publish/os-dev-disk/x86_64/disk/disk.qcow2}
rm -rf -- "$out"
mkdir -p "$out"

set +e
"$vm" scenario "$flow" --json --out "$out" \
  --desktop-disk "$desktop" --timeout "${TILO_FLOW_TIMEOUT:-1800}" "$@" \
  >"$out/run.json" 2>"$out/stderr.txt"
rc=$?
set -e
cat "$out/run.json"
if [ -s "$out/stderr.txt" ]; then cat "$out/stderr.txt" >&2; fi

python3 - "$out/run.json" "$out/result.json" "$rc" <<'PY'
import json
import os
import sys

run, result, rc = sys.argv[1], sys.argv[2], int(sys.argv[3])
if not os.path.exists(result):
    # No step ran: the fleet never came up, or the inputs were refused. That
    # is a failure of this check, not a missing capability.
    try:
        error = json.load(open(run)).get("error", run)
    except Exception:
        error = run
    print(f"the scenario did not reach its steps: {error}", file=sys.stderr)
    sys.exit(rc or 1)

report = json.load(open(result))
steps = report.get("steps", [])
if not steps:
    print("the scenario reported no steps", file=sys.stderr)
    sys.exit(1)
failed = [step for step in steps if step["status"] == "failed"]
missing = [step for step in steps if step["status"] == "missing"]
passed = [step for step in steps if step["status"] == "passed"]
for step in steps:
    detail = step.get("missing") or step.get("detail") or step.get("by", "")
    print(f"  {step['status']:<8} {step['step']:<12} {detail}")
if failed:
    print("steps failed on a capability that exists:", file=sys.stderr)
    for step in failed:
        print(f"  {step['step']}: {step.get('detail')}", file=sys.stderr)
    sys.exit(1)
expected = 1 if (missing or not passed) else 0
if rc != expected:
    print(f"the scenario exited {rc}, expected {expected}", file=sys.stderr)
    sys.exit(1)
print(f"{len(passed)} passed, {len(missing)} missing (named), 0 failed")
PY
