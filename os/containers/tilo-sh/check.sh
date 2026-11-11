#!/usr/bin/env bash
# The local tilo.sh, brought up and used end to end (stack.sh check):
#
#   1. a fresh startup identity, in its own headless Chromium on the local
#      site, approves a server key `tilo keys create` asks for, on the site's
#      own /sign-in page;
#   2-6. with that key, the visa run (sdk/python/examples/visa_proof.py): it
#      makes applicant workspaces, mints a one-workspace key and MCP, the
#      agent fills the form and writes the letter, a reviewer (another real
#      person in their own browser, integration/reviewer.py) joins by the
#      invitation, sees it and takes the agent's run back, and revoking the
#      keys refuses them afterwards;
#   7. a fresh visitor's entry, timed (TILO_SH_ENTRY_VISITS of them, 3 by
#      default), against the Lounge as it is — seeded with `stack.sh up
#      --seed` when TILO_SH_SEED=1.
#
# usage: check.sh [WORK]
# The stack runs this checkout's programs natively, or the images with
# TILO_SH_IMAGES=1 (stack.sh check --images; t's tilo-sh-local check). WORK keeps what it found (pages' consoles and screenshots, the visa run's
# log, report.json); default $TILO_SH_STATE/check. The stack is left up; it
# is stopped first when TILO_SH_FRESH=1 (a new Lounge and door). The tilo
# command is TILO_SH_TILO_BIN, else this checkout's release build the stack
# serves at /releases, else target/{debug,release}/tilo. VISA_APPLICANTS
# sets how many workspaces the run makes (25 here).
# Needs what stack.sh needs, node, a headless Chromium (CHROME or
# Playwright's), and uv (or python3 with httpx) for the Python SDK.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
state=${TILO_SH_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/tilo-sh-local}
base=${TILO_SH_PORT_BASE:-24100}
work=${1:-$state/check}
mkdir -p "$work"
find "$work" -mindepth 1 -delete
site=http://localhost:$((base + 20))
relay=https://127.0.0.1:$base
door=http://localhost:$((base + 10))
began=$SECONDS
step() { printf '\n== %s (%ss)\n' "$*" $((SECONDS - began)); }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
report=$work/report.json
pids=()
trap 'for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done' EXIT

step 'bring the stack up'
up_args=()
[[ ${TILO_SH_IMAGES:-} == 1 ]] && up_args+=(--images)
[[ ${TILO_SH_FRESH:-} == 1 ]] && up_args+=(--fresh)
[[ ${TILO_SH_SEED:-} == 1 ]] && up_args+=(--seed)
bash "$here/stack.sh" up "${up_args[@]}" || fail 'the stack did not come up'
bring_up=$(cat "$state/bring-up-seconds")

# The stack's addresses and helpers (find_tilo, sdk_python).
# shellcheck source=stack.sh
. "$here/stack.sh"
tilo=$(find_tilo)
echo "the tilo command: $tilo"

step '1. a fresh startup identity approves a server key on the local site'
node "$here/integration/person.mjs" "$site" "$relay" "$work/startup" \
  --key 'Visa backend' --tilo "$tilo" --door "$door" > "$work/startup.json" \
  || fail "the startup's key was not approved (see $work/startup)"
key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["key"])' "$work/startup.json")
echo "ok: the startup holds its server key"

step '2-6. the visa run against the local door'
reviewer_port=$((base + 50))
sdk_python "$here/integration/reviewer.py" \
  "$reviewer_port" "$site" "$relay" "$door" "$tilo" "$work" > "$work/reviewer.out" 2> "$work/reviewer.log" &
pids+=($!)
for _ in $(seq 100); do grep -q invitee "$work/reviewer.out" 2>/dev/null && break; sleep 0.2; done
grep -q invitee "$work/reviewer.out" || fail "the reviewer did not start: $(cat "$work/reviewer.log")"
visa_began=$SECONDS
set +e
TILO_DOOR=$door TILO_API_KEY=$key TILO_INVITEE=http://127.0.0.1:$reviewer_port/accept \
  VISA_APPLICANTS=${VISA_APPLICANTS:-25} VISA_REVOKE_SERVER_KEY=1 \
  sdk_python "$repo/sdk/python/examples/visa_proof.py" \
  2>&1 | tee "$work/visa.log"
visa_status=${PIPESTATUS[0]}
set -e
visa_seconds=$((SECONDS - visa_began))
if [[ $visa_status != 0 ]]; then
  echo '--- the reviewer said:' >&2
  tail -60 "$work/reviewer.log" >&2
  # A failing check keeps no outputs: what the reviewer's page said goes
  # into the log.
  for said in "$work"/reviewer-*/console.log; do
    [[ -f $said ]] && { echo "--- $said:" >&2; tail -40 "$said" >&2; }
  done
  fail "the visa run failed (see $work/visa.log)"
fi

step '7. a fresh visitor enters the Lounge, timed'
visits=${TILO_SH_ENTRY_VISITS:-3}
entries=()
for visit in $(seq "$visits"); do
  node "$here/integration/person.mjs" "$site" "$relay" "$work/visitor-$visit" > "$work/visitor-$visit.json" \
    || fail "visitor $visit did not get in (see $work/visitor-$visit)"
  entries+=("$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["joinedMs"])' "$work/visitor-$visit.json")")
done

python3 - "$report" "$bring_up" "$visa_seconds" "${TILO_SH_SEED:-0}" "${entries[@]}" <<'EOF'
import json, statistics, sys
path, bring_up, visa, seeded, *entries = sys.argv[1:]
entries = [int(ms) for ms in entries]
report = {
    "bring_up_seconds": int(bring_up),
    "visa_run_seconds": int(visa),
    "lounge_seeded": seeded == "1",
    "entry_ms": entries,
    "entry_ms_median": statistics.median(entries),
}
json.dump(report, open(path, "w"), indent=2)
print(json.dumps(report))
EOF
step "all of it held (report: $report)"
