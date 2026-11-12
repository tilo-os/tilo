#!/usr/bin/env bash
# The web page and the tilo.sh stack end to end, on one stack: the Lounge's
# relay, the API server, the web entry and the programs this checkout built,
# brought up once by tilo-sh/stack.sh as production's manifests bring them up
# (their images' entrypoints, their environment, production's Caddy in front).
# The site every flow meets is the web entry image's, not a server beside it,
# and the API is the API server. They start once; then every flow runs at the
# same time against them, each as people of its own (a fresh browser profile
# per person, so a fresh root, device and membership), never a stack of its
# own. A flow that hangs is stopped at its own limit and named.
#
# Each flow drives a headless Chromium of its own, not a context of one
# shared browser: every page here draws through WebGPU in software, and a
# browser has one GPU process, so pages sharing a browser wait on each other's
# frames (measured: frames handed to the GPU drawn 21-39 s later, app opens of
# 28-46 s, a page stuck starting its graphics), and the clipboard a setup
# copies its prompt to is the browser's own, which two flows' setups would
# overwrite.
#
#   signin         a fresh startup identity approves on the site a server key
#                  `tilo keys create` asks for (tilo-sh/check.sh's step 1); the
#                  visa run reads the key it made
#   directory      the Lounge lists itself in its public directory as the
#                  deployment named it, with its door and default role, and
#                  every record it read on start read whole
#   boot           the page arrives, joins and opens Notes with no exception
#                  or panic (integration/page-boot.mjs)
#   people-call    two people chat, see each other here and call each other
#                  (integration/people.mjs, TILO_PEOPLE_CASE=call)
#   people-desk    two people; the window menu, a few apps from the search,
#                  the strip panned over Notes and Sheets swiped (case desk)
#   people-menus   two people; a pinch, the text menu, the profile card's
#                  Report a problem, the search chord, and none of it reaching
#                  the browser (case menus)
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
#   visa           the visa run against the API server (check.sh's steps 2-6):
#                  applicant workspaces, a one-workspace key and MCP, the agent
#                  fills the form, a reviewer joins by invitation and takes the
#                  run back, and the revoked keys are refused
#   entry          fresh visitors' entry into the Lounge, timed (check.sh's
#                  step 7)
#   passkey        a visitor keeps themselves with a passkey and signs in with
#                  it again (check.sh's step 7b)
#   wait-list      a stranger at an invitation-only door is kept on its wait
#                  list and confirms through the mail (a second Lounge, whose
#                  door takes people by invitation: the one policy the stack
#                  cannot have at once)
#   onboarding     one stranger's whole way in, on a host of its own whose door
#                  takes people by invitation: the wait list and its
#                  confirmation mail, the invitation an operator sends from the
#                  workspace's own wait list and the mail it carries, the
#                  workspace of their own they land in, their passkey bringing
#                  them back to it, a second workspace they make, and a visitor
#                  with no invitation who still cannot get in
#                  (integration/onboarding.mjs, test-onboarding.sh)
#   restart        the Lounge's relay starts again on its volume, and a person
#                  who was not here reads the chat people-call wrote and
#                  writes in it
#
# Every app the page offers opening from the search on the next frame is the
# desktop surface's own in-process test, not a browser's walk here.
#
# usage: test-e2e.sh BUNDLE WORK [FLOW…]
# BUNDLE is the web desktop bundle the wait list's Lounge serves
# (tools/ci/publish/web-desktop-bundle.sh); the stack's site is
# TILO_SH_SITE's. WORK keeps what each flow found under WORK/<flow>/ and its
# output in WORK/<flow>.log, and the stack's state in WORK/state. FLOW names
# the flows to run (default: all but `reloads`; E2E_FLOWS, space separated,
# too); `visa`
# brings `signin` with it and `restart` brings `people-call`. Needs node,
# chromium, curl, bwrap, the stack's environment (TILO_SH_PROGRAMS,
# TILO_SH_LOUNGE, TILO_SH_SITE, TILO_SH_CADDY naming what t built), and
# TILO_RELAY_BIN, TILO_CRYPTO_BIN, TILO_FONTS_DIR and TILO_CLI_BIN for the wait
# list's Lounge and the setups. E2E_FLOW_SECONDS bounds each flow (default
# 480).
set -euo pipefail
usage='usage: test-e2e.sh BUNDLE WORK [FLOW…]'
lounge_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$lounge_dir/../../.." && pwd)
integration=$lounge_dir/integration
tilo_sh=$repo/os/containers/tilo-sh

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
    people-call|people-desk|people-menus|people-invite)
      TILO_PEOPLE_CASE=${flow#people-} exec node "$integration/people.mjs" "$site_origin" "$relay_origin" \
        "$relay_origin" "$out" "$roles" ;;
    share-link) exec node "$integration/share-link.mjs" "$site_origin" "$relay_origin" "$room" "$out" ;;
    reloads) exec node "$integration/reloads.mjs" "$site_origin" "$out" ;;
    agent-setup|setup-resume)
      exec node "$integration/$flow.mjs" "$site_origin" "$relay_origin" "$TILO_CLI_BIN" "$E2E_SITE_CA" "$out" ;;
    signin|visa|entry|passkey) exec bash "$tilo_sh/check.sh" --flow "$flow" ;;
    wait-list)
      # A second Lounge, whose door takes people by invitation: the one
      # deployment setting the stack above cannot have at once, on addresses
      # of its own (direct-lounge.sh, LOUNGE_PORT_BASE).
      exec bash "$lounge_dir/test-wait-list-door.sh" "$E2E_BUNDLE" "$out" ;;
    onboarding)
      # A host of its own for the same reason, on addresses of its own.
      LOUNGE_PORT_BASE=19810 exec bash "$lounge_dir/test-onboarding.sh" "$E2E_BUNDLE" "$out" ;;
    restart)
      earlier=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["chat"])' \
        "$E2E_WORK/people-call/report.json") || { echo 'FAIL: people-call left no chat message behind'; exit 1; }
      # The Lounge's relay alone, stopped and started again on the volume it
      # kept, the site and the door left serving: what a deployment's restart
      # is.
      bash "$tilo_sh/stack.sh" restart relay
      [[ $(cat "$TILO_SH_STATE/relay/lounge/room-id") == "$room" ]] \
        || { echo 'FAIL: a restart on the same volume served a different workspace'; exit 1; }
      echo "ok: the Lounge started again on its own volume ($room)"
      exec node "$integration/people.mjs" "$site_origin" "$relay_origin" "$relay_origin" "$out" "$roles" "$earlier" ;;
  esac
  exit 2
fi
bundle=${1:?$usage}
work=${2:?$usage}
shift 2
all=(signin directory boot people-call people-desk people-menus people-invite share-link agent-setup setup-resume visa entry passkey wait-list restart)
# Run only when named: a person reloading their workspace page twenty times
# in one browser, counting the reloads that crash or never come back; and a
# stranger's whole onboarding, which stops at the operator's invitation: an
# invitation the host issues after the workspace started again on its volume
# is not answered to the page that asked for it, so the flow waits for a link
# that the host has already made.
named_only=(reloads onboarding)
asked=("$@")
[[ ${#asked[@]} -gt 0 ]] || read -r -a asked <<< "${E2E_FLOWS:-${all[*]}}"
for flow in "${asked[@]}"; do
  [[ " ${all[*]} ${named_only[*]} " == *" $flow "* ]] \
    || { echo "no flow $flow; the flows are: ${all[*]} ${named_only[*]}" >&2; exit 2; }
done
wants() { [[ " ${asked[*]} " == *" $1 "* ]]; }
# The visa run reads the key the startup approved, and the restart reads what
# people-call wrote.
if wants visa && ! wants signin; then asked+=(signin); fi
if wants restart && ! wants people-call; then asked+=(people-call); fi
tilo_bin=${TILO_CLI_BIN:-}
[[ -x $tilo_bin ]] || { echo 'TILO_CLI_BIN names no tilo command (the setups and the startup run it)' >&2; exit 2; }
# How this run paces itself, for a node that draws every page's WebGPU in
# software beside a full node's other jobs (t's): a step of the page drivers
# is 20 s there, not the 5 s a machine with a GPU needs (answers measured 10-50
# s, a window reaching another person's strip 122 s, a search opening 45 s and
# more), each flow has 480 s, and E2E_PARALLEL run at once. A step that hangs
# still fails within its few steps and names itself; each is set by its
# variable.
export TILO_STEP_SECONDS=${TILO_STEP_SECONDS:-20}
limit=${E2E_FLOW_SECONDS:-480}

rm -rf "$work"
mkdir -p "$work"
began=$SECONDS
phase_at=$SECONDS
timing() { printf 'TIMING %s seconds=%s total=%s\n' "$1" "$((SECONDS - phase_at))" "$((SECONDS - began))"; phase_at=$SECONDS; }

# -- the stack, once ---------------------------------------------------------
# The one stack every flow runs against: production's entrypoints and
# environment over this checkout's programs (stack.sh up), from what t built,
# so it compiles nothing. Its site is the web entry image's, its state is this
# run's.
export TILO_SH_STATE=${TILO_SH_STATE:-$work/state}
# The pages here are people arriving at the Lounge to meet on its desktop, so
# the deployment opens none of its apps in front of it.
export TILO_SH_HOME_APP=
# The stack runs no VM host here: what a workspace's own VM adds is
# tilo-sh-local-vm's, on a node with KVM.
export TILO_SH_VM_HOST=${TILO_SH_VM_HOST:-0}
# The site's /releases: the command the flows run, packed as a release packs
# it, so the stack serves a release without this check waiting on the
# release's own statically linked build (cli-x86_64's).
release=$work/release
mkdir -p "$release"
bash "$repo/tools/ci/publish/cli-release.sh" pack "$tilo_bin" "$release/tilo-linux-x86_64.tar.gz"
export TILO_SH_CLI_DIR=$release TILO_SH_TILO_BIN=$tilo_bin
trap 'bash "$tilo_sh/stack.sh" down || true' EXIT
# Its flows' people arrive as strangers and enter the Lounge as guests: the
# door open to anyone, not production's, which takes people by invitation.
TILO_SH_ADMISSION=${TILO_SH_ADMISSION:-open} bash "$tilo_sh/stack.sh" up --fresh
# Sourced after it is up, for its addresses and its helpers (find_tilo,
# sdk_python): the site, the relay and the door every flow meets, and the one
# tilo command they run.
# shellcheck source=../tilo-sh/stack.sh
. "$tilo_sh/stack.sh"
room=$(cat "$state/relay/lounge/room-id")
roles=$state/secrets/roles.json
log=$state/logs/relay.log
bash "$tilo_sh/stack.sh" status || true
timing 'phase=the stack up'

export E2E_WORK=$work E2E_BUNDLE=$bundle E2E_SITE=$site_origin E2E_RELAY=$relay_origin E2E_ROOM=$room \
  E2E_ROLES=$roles E2E_SITE_CA=$state/local-ca.pem E2E_API_URL=$door_origin TILO_CLI_BIN=$tilo_bin

# What each of the stack's services complained about while it ran: each error
# and warning it logged, without its time, with how often. A failing check
# keeps none of its outputs, so where a service stopped answering is said
# here, beside the flows that waited for it.
service_complaints() {
  local name file said
  for name in relay door web tls; do
    file=$state/logs/$name.log
    [[ -s $file ]] || continue
    said=$(sed -E 's/\x1b\[[0-9;]*m//g' "$file" | grep -E '^[0-9T:.-]+Z +(ERROR|WARN) ' \
      | sed -E 's/^[0-9T:.-]+Z +//' | sort | uniq -c | sort -rn | head -20 || true)
    [[ -z $said ]] || { echo "--- $name complained (count, level, where, what):"; echo "$said"; }
  done
}

# Every record a start reads, read whole: a sealed write left out of its
# history, a record it cannot open, or a seeded app that did not settle serves
# people an empty or unloadable app.
verify_records_read() {
  local broken
  broken=$(grep -E 'writes of its history that do not merge|invalid automerge input|duplicate seq|does not open: |open the chat.s channels|seed(ing)? .* failed' "$log" || true)
  [[ -z $broken ]] || {
    echo 'FAIL: a record did not read whole:' >&2
    head -20 <<< "$broken" >&2
    service_complaints >&2
    exit 1
  }
  echo 'ok: every record the Lounge read on start read whole'
}

# run_flow FLOW: FLOW in a session of its own, bounded by its limit.
declare -A flow_started=() flow_of=()
running=()
run_flow() {
  flow_started[$1]=$(date +%s)
  setsid timeout -k 5 "$limit" bash "$0" --flow "$1" > "$work/$1.log" 2>&1 &
  flow_of[$!]=$1
  running+=("$!")
}
# await_flow: the next flow to end, in the order they end, so each one's
# seconds are its own; its result, and on failure what it said.
failed=()
# The machine as a flow ends: load against cores and the CPU pressure (the
# share of the last ten seconds something runnable waited for a core) of the
# machine and of this job, so a slow or timed-out flow on a starved node reads
# differently from one where the product was slow on a machine with room.
machine_now() {
  local load pressure job
  load=$(cut -d' ' -f1 /proc/loadavg 2>/dev/null)
  pressure=$(sed -n 's/^some avg10=\([0-9.]*\).*/\1/p' /proc/pressure/cpu 2>/dev/null)
  job=$(sed -n 's/^some avg10=\([0-9.]*\).*/\1/p' /sys/fs/cgroup/cpu.pressure 2>/dev/null)
  printf 'load %s on %s cores, this job given %s threads, cpu pressure %s%% (machine) %s%% (this job)' \
    "${load:-?}" "$(nproc --all)" "${OMP_NUM_THREADS:-?}" "${pressure:-?}" "${job:-?}"
}
await_flow() {
  local status=0 pid= left=() one
  wait -n -p pid "${running[@]}" || status=$?
  for one in "${running[@]}"; do [[ $one == "$pid" ]] || left+=("$one"); done
  running=("${left[@]}")
  set -- "${flow_of[$pid]}"
  # Whatever the flow left running goes with it.
  kill -KILL -- "-$pid" 2>/dev/null || true
  # Its own seconds: from its start to its log's last word, not to when this
  # run got round to it.
  local took=$(($(stat -c %Y "$work/$1.log") - ${flow_started[$1]}))
  ((status != 124)) || echo "FAIL: $1 did not finish in ${limit}s" >> "$work/$1.log"
  if ((status == 0)); then
    printf 'TIMING flow=%s seconds=%s result=ok (%s)\n' "$1" "$took" "$(machine_now)"
    grep -E '^(TIMING|ok:)' "$work/$1.log" | sed "s/^/  [$1] /" || true
  else
    printf 'TIMING flow=%s seconds=%s result=FAIL (%s)\n' "$1" "$took" "$(machine_now)"
    # A failed check keeps no outputs: where the flow got to, step by step,
    # and what it said last are said here.
    echo "--- $1's steps:"
    grep -E '^(TIMING|ok:)' "$work/$1.log" | sed "s/^/  [$1] /" || true
    echo "--- $1 said (its last 60 lines):"
    tail -60 "$work/$1.log" | sed "s/^/  [$1] /"
    failed+=("$1")
  fi
}

verify_records_read

# The startup identity first, in its own flow: the visa run reads the key its
# page approved.
if wants signin; then
  run_flow signin
  await_flow
fi

parallel=()
for flow in "${asked[@]}"; do [[ $flow == restart || $flow == signin ]] || parallel+=("$flow"); done
# The visa run has nothing to run without the key the startup approved.
if [[ " ${parallel[*]} " == *' visa '* && " ${failed[*]} " == *' signin '* ]]; then
  echo 'TIMING flow=visa seconds=0 result=FAIL (the startup approved no key)'
  failed+=(visa)
  kept=()
  for flow in "${parallel[@]}"; do [[ $flow == visa ]] || kept+=("$flow"); done
  parallel=("${kept[@]}")
fi
# At most E2E_PARALLEL flows at once (default 4), the longest first, the next
# starting as one ends. Every page here draws in software, and a node gives a
# run so many cores: past that, more pages at once make every step of every
# flow slower rather than the run shorter (measured: with nine flows and four
# people cases at once, steps a run of three cases passes in 15-20 s ran out at
# 45 s - a search opening, a composer showing, a call's channel).
order=(setup-resume visa people-call people-invite people-desk people-menus boot wait-list onboarding share-link agent-setup entry passkey directory reloads)
queue=()
for flow in "${order[@]}"; do [[ " ${parallel[*]} " == *" $flow "* ]] && queue+=("$flow"); done
at_once=${E2E_PARALLEL:-4}
for flow in "${queue[@]}"; do
  ((${#running[@]} < at_once)) || await_flow
  run_flow "$flow"
done
while ((${#running[@]})); do await_flow; done
timing "phase=every flow side by side (${parallel[*]})"
if wants restart; then
  if [[ " ${failed[*]} " == *' people-call '* ]]; then
    echo 'TIMING flow=restart seconds=0 result=FAIL (people-call wrote no chat to read back)'
    failed+=(restart)
  else
    run_flow restart
    await_flow
    verify_records_read
    timing 'phase=the Lounge started again on its volume'
  fi
fi

# What the services complained about while the flows ran, on every run,
# passing or not.
service_complaints
printf 'TIMING phase=end seconds=%s total=%s\n' 0 "$((SECONDS - began))"
if ((${#failed[@]})); then
  echo "FAIL: ${failed[*]}"
  exit 1
fi
echo "ok: every flow passed (${asked[*]}) in $((SECONDS - began))s"
