#!/usr/bin/env bash
# The web page end to end, against one Lounge: its own programs run directly
# (direct-lounge.sh) with the entrypoint and environment its image runs them
# with, and the web entry served beside it. They start once; then every flow
# runs at the same time against them, each as people of its own (a fresh
# browser profile per person, so a fresh root, device and membership), never
# a stack of its own. A flow that hangs is stopped at its own limit and named.
#
# Each flow drives a headless Chromium of its own, not a context of one
# shared browser: every page here draws through WebGPU in software, and a
# browser has one GPU process, so pages sharing a browser wait on each other's
# frames (measured: frames handed to the GPU drawn 21-39 s later, app opens of
# 28-46 s, a page stuck starting its graphics), and the clipboard a setup
# copies its prompt to is the browser's own, which two flows' setups would
# overwrite.
#
#   directory      the Lounge lists itself in its public directory as the
#                  deployment named it, with its door and default role, and
#                  every record it read on start read whole
#   boot           the page arrives, joins and opens Notes with no exception
#                  or panic (integration/page-boot.mjs)
#   people-call    two people chat, see each other here and call each other
#                  (integration/people.mjs, TILO_PEOPLE_CASE=call)
#   people-desk    two people; the window menu, a few apps from the search,
#                  the strip panned and swiped, a pinch, the menus and the
#                  search chord, nothing reaching the browser (case desk)
#   people-invite  a person made an inviter while their page is open invites
#                  a third, who arrives from the link (case invite)
#   share-link     a document's link opens that document live for another
#                  person on a phone-sized page; a run's link opens Console
#   agent-setup    a person sets Tilo up through their agent (this commit's
#                  `tilo`), comes back on a reload and signs in again on a
#                  fresh browser
#   setup-resume   a setup lands where it was through reloads, closed tabs,
#                  history, a second tab, a code running out and the network
#                  dropping
#   wait-list      a stranger at an invitation-only door is kept on its wait
#                  list and confirms through the mail (a second Lounge, whose
#                  door takes people by invitation: the one policy the first
#                  cannot have at once)
#   restart        after the rest: the Lounge starts again on its volume, and
#                  a person who was not here reads the chat people-call wrote
#                  and writes in it
#
# Every app the page offers opening from the search on the next frame is the
# desktop surface's own in-process test, not a browser's walk here.
#
# usage: test-e2e.sh BUNDLE WORK [FLOW…]
# BUNDLE is the web desktop bundle (tools/ci/publish/web-desktop-bundle.sh);
# WORK keeps what each flow found under WORK/<flow>/ and its output in
# WORK/<flow>.log. FLOW names the flows to run (default: all; E2E_FLOWS, space
# separated, too); `restart` brings people-call with it. Needs node, chromium,
# curl, TILO_RELAY_BIN, TILO_CRYPTO_BIN, TILO_FONTS_DIR and TILO_CLI_BIN.
# E2E_FLOW_SECONDS bounds each flow (default 300).
set -euo pipefail
usage='usage: test-e2e.sh BUNDLE WORK [FLOW…]'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
integration=$here/integration

# One flow, as the run below starts each: in a session of its own under its
# limit, with what the stack is in the environment (E2E_*), so a flow that
# hangs is stopped with everything it started.
if [[ ${1:-} == --flow ]]; then
  flow=$2 out=$E2E_WORK/$2
  site_origin=$E2E_SITE relay_origin=$E2E_RELAY room=$E2E_ROOM roles=$E2E_ROLES
  case $flow in
    directory)
      page=$(curl -ksS --max-time 5 -X POST "$relay_origin/peer/v1/directory") \
        || { echo 'FAIL: the directory did not answer'; exit 1; }
      E2E_PAGE=$page python3 -c '
import json, os, sys
room = sys.argv[1]
page = json.loads(os.environ["E2E_PAGE"])
listings = page.get("workspaces")
if listings is None or len(listings) != 1:
    sys.exit(f"FAIL: the directory lists {listings and len(listings)} workspaces, not the Lounge alone: {page}")
listing = listings[0]
expected = {
    "name": "Lounge",
    "description": "The Tilo community: chat, notes and a shared desktop",
    "defaultRole": "Editor",
    "door": f"directory/{room}/",
    "room": room,
}
wrong = {field: listing.get(field) for field, want in expected.items() if listing.get(field) != want}
if wrong:
    sys.exit(f"FAIL: the listing differs from the deployment'"'"'s: {wrong} (wanted {expected})")
print("ok: the directory lists the Lounge, joined at its door as an editor")
' "$room"
      exit ;;
    boot) exec node "$integration/page-boot.mjs" "$site_origin" "$relay_origin" "$out" ;;
    people-call|people-desk|people-invite)
      TILO_PEOPLE_CASE=${flow#people-} exec node "$integration/people.mjs" "$site_origin" "$relay_origin" \
        "$relay_origin" "$out" "$roles" ;;
    share-link) exec node "$integration/share-link.mjs" "$site_origin" "$relay_origin" "$room" "$out" ;;
    agent-setup|setup-resume)
      exec node "$integration/$flow.mjs" "$site_origin" "$relay_origin" "$TILO_CLI_BIN" "$E2E_SITE_CA" "$out" ;;
    wait-list)
      LOUNGE_PORT_BASE=$((${LOUNGE_PORT_BASE:-19780} + 100)) exec bash "$here/test-wait-list-door.sh" "$E2E_BUNDLE" "$out" ;;
    restart)
      earlier=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["chat"])' \
        "$E2E_WORK/people-call/report.json") || { echo 'FAIL: people-call left no chat message behind'; exit 1; }
      exec node "$integration/people.mjs" "$site_origin" "$relay_origin" "$relay_origin" "$out" "$roles" "$earlier" ;;
  esac
  exit 2
fi
bundle=${1:?$usage}
work=${2:?$usage}
shift 2
all=(directory boot people-call people-desk people-invite share-link agent-setup setup-resume wait-list restart)
asked=("$@")
[[ ${#asked[@]} -gt 0 ]] || read -r -a asked <<< "${E2E_FLOWS:-${all[*]}}"
for flow in "${asked[@]}"; do
  [[ " ${all[*]} " == *" $flow "* ]] || { echo "no flow $flow; the flows are: ${all[*]}" >&2; exit 2; }
done
wants() { [[ " ${asked[*]} " == *" $1 "* ]]; }
# The restart reads what people-call wrote.
if wants restart && ! wants people-call; then asked+=(people-call); fi
tilo_bin=${TILO_CLI_BIN:-}
if wants agent-setup || wants setup-resume; then
  [[ -x $tilo_bin ]] || { echo 'TILO_CLI_BIN names no tilo command (agent-setup and setup-resume run it)' >&2; exit 2; }
fi
limit=${E2E_FLOW_SECONDS:-300}

rm -rf "$work"
mkdir -p "$work"
began=$SECONDS
phase_at=$SECONDS
timing() { printf 'TIMING %s seconds=%s total=%s\n' "$1" "$((SECONDS - phase_at))" "$((SECONDS - began))"; phase_at=$SECONDS; }

# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
flow_pids=()
finish() {
  local pid
  for pid in "${flow_pids[@]}"; do kill -KILL -- "-$pid" 2>/dev/null || true; done
  stop_lounge
}
trap finish EXIT

start_lounge "$bundle" "$work/lounge"
timing 'phase=the Lounge and its site up'

export E2E_WORK=$work E2E_BUNDLE=$bundle E2E_SITE=$site_origin E2E_RELAY=$relay_origin E2E_ROOM=$room \
  E2E_ROLES=$roles E2E_SITE_CA=$work/lounge/site.ca.crt TILO_CLI_BIN=$tilo_bin

# Every record a start reads, read whole: a sealed write left out of its
# history, a record it cannot open, or a seeded app that did not settle serves
# people an empty or unloadable app.
verify_records_read() {
  local broken
  broken=$(grep -E 'writes of its history that do not merge|invalid automerge input|duplicate seq|does not open: |open the chat.s channels|seed(ing)? .* failed' "$log" || true)
  [[ -z $broken ]] || lounge_fail "a record did not read whole: $(head -5 <<< "$broken")"
  echo 'ok: every record the Lounge read on start read whole'
}
verify_records_read

# run_flow FLOW: FLOW in a session of its own, bounded by its limit.
declare -A flow_started=() flow_of=()
running=()
run_flow() {
  flow_started[$1]=$SECONDS
  setsid timeout -k 5 "$limit" bash "$0" --flow "$1" > "$work/$1.log" 2>&1 &
  flow_of[$!]=$1
  flow_pids+=("$!")
  running+=("$!")
}
# await_flow: the next flow to end, in the order they end, so each one's
# seconds are its own; its result, and on failure what it said.
failed=()
await_flow() {
  local status=0 pid= left=() one
  wait -n -p pid "${running[@]}" || status=$?
  for one in "${running[@]}"; do [[ $one == "$pid" ]] || left+=("$one"); done
  running=("${left[@]}")
  set -- "${flow_of[$pid]}"
  # Whatever the flow left running goes with it.
  kill -KILL -- "-$pid" 2>/dev/null || true
  local took=$((SECONDS - ${flow_started[$1]}))
  ((status != 124)) || echo "FAIL: $1 did not finish in ${limit}s" >> "$work/$1.log"
  if ((status == 0)); then
    printf 'TIMING flow=%s seconds=%s result=ok\n' "$1" "$took"
    grep '^ok:' "$work/$1.log" | sed "s/^/  [$1] /" || true
  else
    printf 'TIMING flow=%s seconds=%s result=FAIL\n' "$1" "$took"
    # A failed check keeps no outputs: where the flow got to, step by step,
    # and what it said last are said here.
    echo "--- $1's steps:"
    grep -E '^(TIMING|ok:)' "$work/$1.log" | sed "s/^/  [$1] /" || true
    echo "--- $1 said (its last 60 lines):"
    tail -60 "$work/$1.log" | sed "s/^/  [$1] /"
    failed+=("$1")
  fi
}

parallel=()
for flow in "${asked[@]}"; do [[ $flow == restart ]] || parallel+=("$flow"); done
for flow in "${parallel[@]}"; do run_flow "$flow"; done
for _ in "${parallel[@]}"; do await_flow; done
timing "phase=every flow side by side (${parallel[*]})"
if wants restart; then
  if [[ " ${failed[*]} " == *' people-call '* ]]; then
    echo 'TIMING flow=restart seconds=0 result=FAIL (people-call wrote no chat to read back)'
    failed+=(restart)
  else
    # In this shell, not a flow's: the Lounge it starts again is this run's
    # to stop.
    restart_lounge
    verify_records_read
    timing 'phase=the Lounge started again on its volume'
    run_flow restart
    await_flow
  fi
fi

# What the Lounge complained about while it ran: each error and warning it
# logged, without its time, with how often, on every run, passing or not.
said=$(sed -E 's/\x1b\[[0-9;]*m//g' "$log" | grep -E '^[0-9T:.-]+Z +(ERROR|WARN) ' \
  | sed -E 's/^[0-9T:.-]+Z +//' | sort | uniq -c | sort -rn | head -40 || true)
[[ -z $said ]] || { echo '--- the Lounge complained (count, level, where, what):'; echo "$said"; }
printf 'TIMING phase=end seconds=%s total=%s\n' 0 "$((SECONDS - began))"
if ((${#failed[@]})); then
  echo "FAIL: ${failed[*]}"
  exit 1
fi
echo "ok: every flow passed (${asked[*]}) in $((SECONDS - began))s"
