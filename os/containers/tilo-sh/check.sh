#!/usr/bin/env bash
# The local tilo.sh, brought up and used end to end (stack.sh check):
#
#   1. a fresh startup identity, in its own headless Chromium on the local
#      site, approves a server key `tilo do account/keys#create` asks for, on the site's
#      own /sign-in page; the pictures of what it reads there (the sign-in
#      card, the API server's grants page, and the key that page makes) are kept in
#      WORK/startup, light and dark. The page stays open on the site, holding
#      that person, so it is where their hosts' questions are answered;
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
# With TILO_SH_VM_HOST=1 it is the same stack with its VM host in it, and the
# end of the check is the Browser of a hosted workspace (docs/hosted-browser.md),
# in the workspace's own VM:
#
#   8. the visa test page (crates/engines/chromium-host/test-pages/visa-form.html), served
#      where a VM's network may reach it: the server endpoint's own address and
#      port, the one destination besides the internet a VM is given;
#   9. the guest this stack's VM host boots carries the Browser there: the
#      headless session and the image's unprivileged browser user;
#  9b. the host warms the template that size's VMs start from, whose first boot
#      is cold by necessity;
#  10. a key opens Browser in a hosted workspace, timed to a live window, in a
#      VM that woke as a clone of that template (the host's own measure of the
#      wake is printed), never a cold boot of the guest image;
#  11. the VM the Browser is in, with the visa test page open in its window:
#      every renderer sandboxed, the session running as the image's browser
#      user, its device key, and what the page costs in memory
#      (integration/vm-proofs.py);
#  12. the agent fills the visa test page through page actions with the
#      person asked nothing (their key operates web pages), captures it, and
#      a person invited into the workspace, in their own browser, sees that
#      page on their shell — the agent's next fill changes the picture they
#      are looking at — before anyone is asked to approve anything; then the
#      submission runs once the person allows that one submission, the
#      receipt reaches the same shell, and a second submission is refused when
#      they deny it — both answered by the person on the question sheet of
#      their own page, open on the site since step 1, what each sheet drew
#      kept beside its answer in WORK/startup/answers.jsonl and their view of
#      it as WORK/startup/submission-{denied,allowed}-{asked,answered}.png —
#      then a submission is allowed on a browser signed in through a ticket,
#      holding none of their identity, and another allowed and one denied on
#      another browser of theirs, signed in by that page and open in the
#      applicant's workspace while their root's page shows another: the
#      question reaches that browser, and a denial there is refused to the
#      agent with the refusal's own code; and
#      a click on the submit button is held and asked once; a key minted with
#      the scope unticked is asked about the page actions that act on the
#      page; a reviewer who joined by invitation reads the run in History, and
#      page actions past the plans file's free ones are refused;
#  13. a Linux command in that workspace's VM, from an agent's run, while the
#      provider's plans file names no card for the account: refused at once,
#      in words that say what would let it through, having run nothing
#      (integration/hosted-terminal.py);
#  14. the provider puts the account on file, and the Browser's page action
#      the allowance refused runs;
#  15. the workspace's Terminal, as the person's own key runs it: a file
#      made, `ls` and `uname` answered in the VM, the run kept as a structured
#      record of its session and as a step of History named after that key;
#  16. an agent's key, scoped to that one workspace, running a command there
#      through the `tilo` command (the MCP behind it), and its own step of
#      History;
#  17. the Linux session runs as the VM's tenant, not as the account its
#      Browser session runs as, and neither reads the other's files;
#  18. a Linux GUI app in that workspace's VM: the agent opens the guest's X11
#      terminal, a person invited into the workspace sees its window appear on
#      their shell in their own browser and types a command into it, and the
#      file the command wrote is read back from the VM; the app runs as the
#      tenant, not as Browser's account, and the VM's memory with it open is
#      measured, in the guest and on the host.
#
# usage: check.sh [WORK]
# The stack runs this checkout's programs natively (the outputs a check hands
# it: TILO_SH_PROGRAMS, TILO_SH_LOUNGE, TILO_SH_SITE, TILO_SH_CLI_DIR,
# TILO_SH_CADDY, or this machine's own build). WORK keeps what it found
# (pages' consoles and screenshots, the visa run's
# log, report.json); default $TILO_SH_STATE/check. The stack is left up; it
# is stopped first when TILO_SH_FRESH=1 (a new Lounge and entry). The tilo
# command is TILO_SH_TILO_BIN, else this checkout's release build the stack
# serves at /releases, else target/{debug,release}/tilo. VISA_APPLICANTS
# sets how many workspaces the run makes (25 here). The questions put to the
# startup person are answered on their page, which step 1 keeps open on the
# site and whose answers the check writes to and reads from
# (integration/page_person.py), so it is opened whenever a step that asks runs.
# TILO_SH_ONLY names the steps to run (`1`, `2-6`, `7`, `10,13-18`, comma
# separated) to chase one of them without the rest; the stack is brought up
# either way and each step's own seconds are printed beside it.
#
#   check.sh --flow signin|visa|entry
#
# runs one of those steps against a stack that is already up and named by
# E2E_SITE, E2E_SERVER, E2E_API_URL and E2E_WORK, which is how test-e2e.sh runs
# them beside the page's own flows; `visa` reads the key `signin` approved,
# from E2E_WORK/signin/startup.json.
#
# Needs what stack.sh needs, node, a headless Chromium (CHROME or
# Playwright's), and uv (or python3 with httpx) for the Python SDK. With
# TILO_SH_VM_HOST=1 the stack also runs its VM host, from the images
# TILO_SH_IMAGE_VM_HOST and TILO_SH_HOSTED_GUEST name: that needs /dev/kvm,
# root, podman and a loop device, and the guest image must carry the headless
# browser session.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
state=${TILO_SH_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/tilo-sh-local}
base=${TILO_SH_PORT_BASE:-24100}
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pids=()
trap 'for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done' EXIT
# The Python SDK as this checkout has it, from stack.sh (sourced below).
key= plain_key= person_asks= reviewer_port= entries=() visa_seconds=0 at=$SECONDS began=$SECONDS
step() {
  printf '\n== %s (%ss, %ss in)\n' "$*" $((SECONDS - at)) $((SECONDS - began))
  at=$SECONDS
}

# -- the steps ---------------------------------------------------------------

# 1: a person who has never been here, on the site, approves the server key
# `tilo do account/keys#create` asks for, and one that may not operate web pages. The
# site lets people in by invitation, as production's does: they arrive through
# one the Lounge's owner made for them (`stack.sh invite`), as a newcomer does.
startup_identity() {
  step '1. a fresh startup identity approves a server key on the local site'
  local invitation
  invitation=$(invite_link) || fail 'the Lounge made no invitation for the startup'
  local args=(--invite "$invitation" --key 'Visa backend' --tilo "$tilo" --api "$api" --no-pages)
  if [[ ${TILO_SH_VM_HOST:-0} == 1 ]] && runs_any 12 13 14 15 16 17 18; then
    # The person stays on the site in their browser, whose page holds their
    # identity: the hosted steps have them answer what their agent's page and
    # their workspaces' VMs put to them, on its question sheet. The check
    # holds the FIFO open so each answer asked of it is one line, not its end.
    mkdir -p "$work/startup"
    person_asks=$work/startup/asks
    rm -f "$person_asks" "$work/startup/answers.jsonl"
    mkfifo "$person_asks"
    exec {person_asks_fd}<>"$person_asks"
    node "$here/integration/person.mjs" "$site" "$server" "$work/startup" "${args[@]}" \
      --answer "$person_asks" > "$work/startup.json" 2> "$work/startup.log" &
    local person=$!
    pids+=("$person")
    until [[ -s $work/startup.json ]]; do
      kill -0 "$person" 2>/dev/null || { cat "$work/startup.log" >&2; fail "the startup's key was not approved (see $work/startup)"; }
      sleep 0.5
    done
    cat "$work/startup.log" >&2
  else
    node "$here/integration/person.mjs" "$site" "$server" "$work/startup" "${args[@]}" > "$work/startup.json" \
      || fail "the startup's key was not approved (see $work/startup)"
  fi
  key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["key"])' "$work/startup.json")
  # The key they did not let operate web pages, made on the API server's grants page:
  # what the last step holds to being asked about each page action.
  plain_key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["noPagesKey"])' "$work/startup.json")
  [[ -n $plain_key ]] || fail 'the startup made no key without the Browser scope (see $work/startup)'
  echo "ok: the startup holds its server key, and one that may not operate web pages"
}

# The reviewer's helper: each invitation posted to it is another person in
# their own headless Chromium, up for every run that invites someone.
reviewer_up() {
  reviewer_port=$((base + 50))
  sdk_python "$here/integration/reviewer.py" \
    "$reviewer_port" "$site" "$server" "$api" "$tilo" "$work" > "$work/reviewer.out" 2> "$work/reviewer.log" &
  pids+=($!)
  for _ in $(seq 100); do grep -q invitee "$work/reviewer.out" 2>/dev/null && break; sleep 0.2; done
  grep -q invitee "$work/reviewer.out" || fail "the reviewer did not start: $(cat "$work/reviewer.log")"
}

# 2-6: the visa run against the API server, with the reviewer's helper up.
visa_run() {
  step '2-6. the visa run against the local API server'
  local visa_began=$SECONDS status
  set +e
  TILO_API_URL=$api TILO_API_KEY=$key TILO_INVITEE=http://127.0.0.1:$reviewer_port/accept \
    VISA_APPLICANTS=${VISA_APPLICANTS:-25} VISA_REVOKE_SERVER_KEY=1 \
    sdk_python "$repo/sdk/python/examples/visa_proof.py" \
    2>&1 | tee "$work/visa.log"
  status=${PIPESTATUS[0]}
  set -e
  visa_seconds=$((SECONDS - visa_began))
  if [[ $status != 0 ]]; then
    echo '--- the reviewer said:' >&2
    tail -60 "$work/reviewer.log" >&2
    # A failing check keeps no outputs: what the reviewer's page said goes
    # into the log.
    local said
    for said in "$work"/reviewer-*/console.log; do
      [[ -f $said ]] && { echo "--- $said:" >&2; tail -40 "$said" >&2; }
    done
    fail "the visa run failed (see $work/visa.log)"
  fi
}

# 7: fresh visitors enter the Lounge, each timed, each through an invitation
# of their own, as the site lets newcomers in.
entry_visits() {
  step '7. a fresh visitor enters the Lounge, timed'
  local visits=${TILO_SH_ENTRY_VISITS:-3} visit median
  entries=()
  for visit in $(seq "$visits"); do
    local invitation
    invitation=$(invite_link) || fail "the Lounge made no invitation for visitor $visit"
    node "$here/integration/person.mjs" "$site" "$server" "$work/visitor-$visit" --invite "$invitation" \
      > "$work/visitor-$visit.json" \
      || fail "visitor $visit did not get in (see $work/visitor-$visit)"
    entries+=("$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["joinedMs"])' "$work/visitor-$visit.json")")
  done
  median=$(python3 -c 'import statistics,sys; print(int(statistics.median(int(ms) for ms in sys.argv[1:])))' "${entries[@]}")
  echo "ok: $visits fresh visitors were in after ${entries[*]} ms (median $median ms)"
}

# -- one step, for a runner that keeps the stack up --------------------------

if [[ ${1:-} == --flow ]]; then
  flow=${2:?--flow names signin, visa or entry}
  work=${E2E_WORK:?E2E_WORK names the work directory}"/$flow"
  site=${E2E_SITE:?E2E_SITE names the site} server=${E2E_SERVER:?E2E_SERVER names the server}
  api=${E2E_API_URL:?E2E_API_URL names the API server}
  mkdir -p "$work"
  # The stack's addresses and helpers, and the tilo command it serves.
  # shellcheck source=stack.sh
  . "$here/stack.sh"
  tilo=$(find_tilo)
  case $flow in
    signin) startup_identity ;;
    visa)
      key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["key"])' \
        "${E2E_WORK:?}/signin/startup.json") \
        || fail "the startup approved no key (see ${E2E_WORK:?}/signin)"
      reviewer_up
      visa_run ;;
    entry) entry_visits ;;
    *) fail "no step $flow: signin, visa or entry" ;;
  esac
  step "done ($flow)"
  exit 0
fi

# -- the whole check, on the stack it brings up ------------------------------

work=${1:-$state/check}
mkdir -p "$work"
find "$work" -mindepth 1 -delete
site=http://tilo.localhost:$((base + 20))
server=http://127.0.0.1:$base
api=http://localhost:$((base + 10))
report=$work/report.json

# TILO_SH_ONLY names the steps to run, comma separated (`1`, `2-6`, `7`): what
# a step that failed is chased with, instead of the whole check around it.
# Naming none runs them all. A step that reads what an earlier one made is run
# with that one: the visa run (2-6) and the hosted Browser and its Linux
# commands (8-18) read the key and the person the startup made in 1, and the
# Linux commands read the workspace the open made in 10, so naming any of them
# wants those too (`TILO_SH_ONLY=10,13-18`). The stack is brought up either
# way.
only=${TILO_SH_ONLY:-}
runs_step() {
  [[ -z $only ]] && return 0
  local named number
  for named in ${only//,/ }; do
    if [[ $named == *-* ]]; then
      for number in $(seq "${named%-*}" "${named#*-}"); do [[ $1 == "$number" ]] && return 0; done
    else
      [[ $1 == "$named" ]] && return 0
    fi
  done
  return 1
}
runs_any() { local number; for number in "$@"; do runs_step "$number" && return 0; done; return 1; }

step 'bring the stack up'
up_args=()
[[ ${TILO_SH_FRESH:-} == 1 ]] && up_args+=(--fresh)
[[ ${TILO_SH_SEED:-} == 1 ]] && up_args+=(--seed)
# Its visitors arrive as strangers and enter the Lounge as guests: the entry
# open to anyone, not production's, which takes people by invitation.
TILO_SH_ADMISSION=${TILO_SH_ADMISSION:-open} bash "$here/stack.sh" up "${up_args[@]}" || fail 'the stack did not come up'
bring_up=$(cat "$state/bring-up-seconds")

# The stack's addresses and helpers (find_tilo, sdk_python).
# shellcheck source=stack.sh
. "$here/stack.sh"
tilo=$(find_tilo)
echo "the tilo command: $tilo"

# The server that came up keeps no sealed store of its own (stack.sh vaults).
host_keeps_no_vault || fail "the server keeps a sealed store of its own beside its workspaces' vaults"

# The Lounge's moderation bot is the operator's grant, written once when a
# fresh stack first brings the workspace up. A person who governs may turn it
# off afterwards, and a restart leaves their choice: so only a fresh stack is
# held to the grant, and one brought up over state a person changed is not.
# The server says the grant is there on every start while it is. The
# in-process test of the same start is
# `a_public_workspace_is_granted_its_moderation_bot_at_start`.
if [[ ${TILO_SH_FRESH:-} == 1 ]]; then
  grep -q 'the moderation bot is enabled for this workspace' "$state/logs/server.log" \
    || fail "the fresh Lounge has no moderation grant: $(tail -20 "$state/logs/server.log" 2>/dev/null)"
  say 'the fresh Lounge has its moderation grant'
else
  say "the Lounge keeps the moderation setting its person last chose"
fi

if runs_any 1 2 3 4 5 6 8 9 10 11 12 13 14 15 16 17 18; then
  startup_identity
fi

if runs_any 2 3 4 5 6 12; then
  reviewer_up
fi

# The hosted Browser's checks (8-11) run here, on that key: the visa run below
# ends by revoking it.
hosted=${TILO_SH_VM_HOST:-0}
hosted_report=$work/hosted/report.json
if [[ $hosted == 1 ]]; then
  [[ -e /dev/kvm ]] || fail '/dev/kvm: the hosted Browser runs in a VM, and this machine has none'
  # What the server reads to place here: the account a card is on file for,
  # and the address a VM's browser session reaches the server at.
  vm_env=$(vm_host env 2>/dev/null) || true
  vm_account=$(sed -n 's/^TILO_HOSTED_ACCOUNT=//p' <<<"$vm_env")
  server_address=$(sed -n 's/^TILO_HOSTED_BROWSER_SERVER=//p' <<<"$vm_env")
  [[ -n $vm_account ]] || fail 'the VM host is not up, so the server places no Browser: stack.sh status says why'
  plans=$state/secrets/vm-host-plans.json
  # The plans file the provider would edit is the VM host's, made as root: the
  # check plays the provider, adding the account to it mid-run.
  [[ -w $plans ]] || sudo -n chown "$(id -u):$(id -g)" "$plans" 2>/dev/null \
    || fail "the plans file $plans is not the check's to edit"
  # The proofs run in the workspace's own VM, and a VM belongs to the account
  # the server placed it for: the account this stack admits by. The guest check
  # below runs as an account with a card on file, so a probe of its own admits.
  vm_run_account=$(python3 -c 'import json,sys; print(next(iter(json.load(open(sys.argv[1])).get("accounts") or {})))' "$plans") \
    || fail "the plans file $plans names no account with a card"
  [[ -n $server_address ]] || fail 'the VM host names no server address for Browser sessions'
  # Steps 12-18 ask the person (a submission held, a Linux command outside the
  # workspace's records): their page answers, and step 1 opened it for them.
  if runs_any 12 13 14 15 16 17 18; then
    [[ -n ${person_asks:-} ]] || fail "the person's page is not open to answer these steps (WORK/startup)"
  fi

  # A Linux command in the VM needs the card the check puts on file in step
  # 14: without it a person's own command waits for the card rather than
  # running, and the step that runs one would sit there. Naming the commands
  # without the steps that add the card says so instead.
  if runs_any 15 16 17 18 && ! runs_any 13 14; then
    python3 -c 'import json,sys; sys.exit(0 if sys.argv[2] in (json.load(open(sys.argv[1])).get("accounts") or {}) else 1)' \
        "$plans" "$vm_account" \
      || fail "the plans file $plans names no card for $vm_account: run steps 13 and 14 too (TILO_SH_ONLY=10,13-18)"
  fi

  if runs_any 8 10 12; then
    step '8. the visa test page, where a VM may reach it'
    form_dir=$work/form
    mkdir -p "$form_dir"
    cp "$repo/crates/engines/chromium-host/test-pages/visa-form.html" "$form_dir/visa-form.html"
    # A VM reaches the public internet and one address of this machine: the
    # server's own endpoint, as the VM dials it (10.0.2.4, which pasta maps to
    # this machine's loopback). The server's endpoint is UDP; its port's TCP side
    # is the stack's to serve, and this is the page it serves.
    form_port=$iroh_port
    python3 -m http.server --bind 127.0.0.1 --directory "$form_dir" "$form_port" \
      > "$work/form.log" 2>&1 &
    pids+=($!)
    form_url=http://10.0.2.4:$form_port/visa-form.html
    for _ in $(seq 100); do
      curl -fsS --max-time 1 "http://127.0.0.1:$form_port/visa-form.html" >/dev/null 2>&1 && break
      sleep 0.1
    done
    curl -fsS --max-time 2 "http://127.0.0.1:$form_port/visa-form.html" >/dev/null \
      || fail "the page server did not answer on 127.0.0.1:$form_port (see $work/form.log); a VM reaches it at 10.0.2.4:$form_port"
    echo "ok: the test page is at $form_url, the one address besides the internet a VM reaches"
  fi

  if runs_step 9; then
    step '9. does the guest this stack boots carry the Browser?'
    # At the size a workspace's VM runs at, so the VM that runs here warms the
    # template step 9b waits for and step 10's workspace VM starts as a clone
    # of. A probe at another size would warm a template nothing clones.
    guest_says=$(sudo -n podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 \
        --init-groups --inh-caps=-all tilo-linux-vm hosted run --run /run/tilo-hosted \
        --workspace ws-browser-guest --account "$vm_run_account" \
        --size "${TILO_SH_HOSTED_VM_SIZE:-default}" -- /bin/sh -c \
        'if [ -x /usr/libexec/tilo/browser-session ]; then echo has-session; else echo no-session; fi
         if getent passwd tilo-browser >/dev/null; then echo has-user; else echo no-user; fi' 2>&1) \
      || fail "the workspace VM did not run the guest's own check: $guest_says"
    echo "$guest_says"
    if ! grep -qx 'has-session' <<<"$guest_says" || ! grep -qx 'has-user' <<<"$guest_says"; then
      fail "the workspace guest this stack boots carries no hosted Browser: a VM needs /usr/libexec/tilo/browser-session (the headless session a first browser action starts) and the image's unprivileged tilo-browser user, and this guest has $(grep -qx 'has-session' <<<"$guest_says" || echo no) session and $(grep -qx 'has-user' <<<"$guest_says" || echo no) user. The Browser of a hosted workspace runs in the workspace's own VM, so this check can go no further until the guest that carries it is in this tree (os/Containerfile.hosted-guest)."
    fi
    echo "ok: the guest carries the headless Browser session and its user"
  fi

  # A workspace's VM starts as a clone of the host's warm template, which the
  # host makes for a size once a VM of that size has run (step 9's). Its own
  # first boot is cold by necessity, and a machine without the room behind it
  # makes none at all, which the host's log says.
  if runs_any 10 11 12; then
    step '9b. the host warms the template a workspace VM starts from'
    warmed=$SECONDS
    template=
    # The host boots a guest, warms its page cache and snapshots it, after the
    # VM step 9 ran has given its memory back: the host's own patience for a
    # guest to answer is five minutes, so waiting less than that here would
    # call a slow warm a failed one. The host logs how long it took.
    deadline=$((SECONDS + 360))
    while ((SECONDS < deadline)); do
      template=$(sudo -n podman logs tilo-sh-vm-host-0 2>&1 | sed -n 's/.*template is ready$/ready/p' | head -1)
      [[ -z $template ]] || break
      sleep 1
    done
    [[ -n $template ]] \
      || fail "the VM host made no warm template in 360 s, so every workspace's VM boots cold: what it said is in sudo -n podman logs tilo-sh-vm-host-0 (a template is admitted at the size's whole memory, and needs a clone's room free; the host's log says how warming went)"
    echo "ok: the template was ready in $((SECONDS - warmed))s"
  fi

  hosted_workspace= hosted_seconds=0
  hosted_flow() { # PHASE
    TILO_INVITEE=http://127.0.0.1:$reviewer_port/accept \
      HOSTED_VISA_PLANS=$plans HOSTED_VISA_ACCOUNT=$vm_account \
      HOSTED_VISA_UNSCOPED_KEY=$plain_key \
      HOSTED_VISA_SITE=$site HOSTED_VISA_SITE_SERVER=$server HOSTED_VISA_TILO=$tilo \
      HOSTED_VISA_PAGE_ASKS=${person_asks:-} HOSTED_VISA_PAGE_ANSWERS=${person_asks:+$work/startup/answers.jsonl} \
      HOSTED_VISA_PAGE_WORKSPACE=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("workspace") or "")' "$work/startup.json") \
      sdk_python "$here/integration/hosted-visa.py" "$api" "$key" "$work/hosted" "$form_url" "$1"
  }
  # A Linux command in that workspace's VM, as its people and their agents
  # run them (integration/hosted-terminal.py).
  hosted_terminal() { # PHASE
    HOSTED_TERMINAL_CLI=$tilo HOSTED_TERMINAL_API_URL=$api \
      HOSTED_TERMINAL_PLANS=$plans HOSTED_TERMINAL_ACCOUNT=$vm_account \
      HOSTED_TERMINAL_ASKS=${person_asks:-} HOSTED_TERMINAL_ANSWERS=$work/startup/answers.jsonl \
      HOSTED_TERMINAL_SITE=$site HOSTED_TERMINAL_SITE_SERVER=$server \
      sdk_python "$here/integration/hosted-terminal.py" "$api" "$key" "$work/hosted" "$1"
  }
  launch=$SECONDS
  if runs_step 10; then
    step '10. a key opens Browser in a hosted workspace, in its VM'
    set +e
    hosted_flow --open 2>&1 | tee "$work/hosted-open.log"
    hosted_status=${PIPESTATUS[0]}
    set -e
    hosted_seconds=$((SECONDS - launch))
    if [[ $hosted_status != 0 ]]; then
      echo '--- the VM host said:' >&2
      sudo -n podman logs --tail 80 tilo-sh-vm-host-0 >&2 2>/dev/null || true
      echo '--- the key the host joined with:' >&2
      sudo -n podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 \
        --init-groups --inh-caps=-all -- env HOME=/var/lib/tilo-hosted/member/home \
        XDG_STATE_HOME=/var/lib/tilo-hosted/member/profile/state \
        TILO_IDENTITY_ROOT=/var/lib/tilo-hosted/member/identity \
        tilo device key >&2 2>/dev/null || true
      sudo -n podman exec tilo-sh-vm-host-0 sh -c \
        'ls -la --time-style=full-iso /var/lib/tilo-hosted/member/identity/device-private /var/lib/tilo-hosted/member/identity/device' >&2 2>/dev/null || true
      echo '--- the member, from its start:' >&2
      sudo -n podman logs tilo-sh-vm-host-0 2>&1 | grep -v 'net_report' | head -60 >&2 || true
      # What the Browser session in the workspace's VM wrote, which the host
      # keeps beside the VM's state.
      echo '--- the Browser session in the VM said:' >&2
      sudo -n podman exec tilo-sh-vm-host-0 sh -c \
        'find /var/lib/tilo-hosted -name browser.log -exec sh -c "echo \"== \$1\"; tail -n 80 \"\$1\"" _ {} \;' \
        >&2 2>/dev/null || true
      echo '--- the server said:' >&2
      bash "$here/stack.sh" logs server 2>/dev/null | tail -40 >&2 || true
      # What the host's container was given to trust, and what it mounted: a
      # member that cannot reach the server says one of these.
      echo '--- what the VM host trusts and mounts:' >&2
      sudo -n podman inspect --format '{{range .Mounts}}{{.Source}} -> {{.Destination}}{{"\n"}}{{end}}' \
        tilo-sh-vm-host-0 >&2 2>/dev/null || true
      sudo -n podman exec tilo-sh-vm-host-0 sh -c \
        'grep -c "BEGIN CERTIFICATE" /etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem 2>&1 || true' >&2 2>/dev/null || true
      # The session in the VM writes what it said beside its VM's files: a
      # session that exited before its window came up says why there.
      echo '--- the browser session in the VM said:' >&2
      sudo -n podman exec tilo-sh-vm-host-0 sh -c \
        'find /var /srv -name "browser*.log" 2>/dev/null | while read -r log; do echo "$log:"; cat "$log"; done' \
        >&2 2>/dev/null || true
      echo '--- what the server said of the Browser:' >&2
      bash "$here/stack.sh" logs server 2>/dev/null \
        | grep -iE 'browser|environment|hosted|placed|peer session|refus|enrol|way in|device' | tail -80 >&2 || true
      echo '--- the server said, to its end:' >&2
      bash "$here/stack.sh" logs server 2>/dev/null | tail -60 >&2 || true
      echo '--- the API server said, to its end:' >&2
      bash "$here/stack.sh" logs api 2>/dev/null | tail -60 >&2 || true
      echo '--- the member said, to its end:' >&2
      sudo -n podman logs --tail 60 tilo-sh-vm-host-0 2>&1 | grep -v 'net_report' >&2 || true
      echo '--- what the guest was shown of the workspace:' >&2
      sudo -n podman exec tilo-sh-vm-host-0 sh -c \
        'ls -la --time-style=full-iso /srv/tilo-member/home/Tilo 2>&1; find /srv/tilo-member/home -maxdepth 4 -name browser -type d 2>/dev/null | head -3' \
        >&2 2>/dev/null || true
      fail "Browser did not open in the workspace's VM (see $work/hosted-open.log)"
    fi
    hosted_workspace=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["workspace"])' "$hosted_report")
    # How that VM woke, as the host measured it: a clone of the host's warm
    # template, which is what makes a workspace's first Browser fast, or a
    # cold boot of the guest image, which is the whole boot a person waits
    # through. The host's line is `<workspace>/<environment> … wake <ms> ms
    # (restored|booted) snapshot …`.
    woke=$(python3 -c '
import re, sys
workspace = sys.argv[1]
line = ""
for candidate in sys.stdin.read().splitlines():
    fields = candidate.split()
    if fields and (fields[0] == workspace or fields[0].startswith(workspace + "/")):
        line = candidate
        break
found = re.search(r"wake (.*?) snapshot", line)
print(found.group(1) if found else "")' "$hosted_workspace" <<<"$(vm_host status 2>/dev/null)")
    [[ -n $woke ]] || fail "the VM host says nothing of $hosted_workspace's wake: vm-host.sh status"
    echo "ok: $hosted_workspace woke by $woke"
    [[ $woke == *'(restored)' ]] \
      || fail "$hosted_workspace woke by $woke: a workspace's VM starts as a clone of the host's warm template, and this one booted the guest image cold (the host's log says whether it could make a template)"
  fi

  if runs_any 11 12; then
    step '11. the VM the Browser ran in: its sandbox, its user, its memory'
    # A VM whose memory the host is saving takes no new run until it is done,
    # and saving it takes as long as the machine holding it takes to write:
    # wait for it to run again, then the proof's own run wakes it if it slept.
    vm_phase() {
      vm_host status 2>/dev/null \
        | awk -v ws="$hosted_workspace/default" '$1 == ws {print $3; exit}'
    }
    waited=$SECONDS
    while [[ $(vm_phase) != Running ]] && ((SECONDS - waited < 600)); do sleep 2; done
    [[ $(vm_phase) == Running ]] \
      || fail "$hosted_workspace's VM is not running again in 600 s (vm_host status says $(vm_phase))"
    # What the proofs read is an engine that renders: the window the open made
    # shows the visa test page, which is what a person and their agent see
    # there before anything is asked of them.
    if ! hosted_flow --page 2>&1 | tee "$work/hosted-page.log"; then
      echo '--- the server said of the page action:' >&2
      bash "$here/stack.sh" logs server 2>/dev/null \
        | grep -iE 'page action|person|ask|confirm|browser|environment' | tail -60 >&2 || true
      echo '--- the API said, to its end:' >&2
      bash "$here/stack.sh" logs api 2>/dev/null | tail -40 >&2 || true
      echo '--- the Browser session in the VM said:' >&2
      sudo -n podman exec tilo-sh-vm-host-0 sh -c \
        'find /var /srv -name "browser*.log" 2>/dev/null | while read -r log; do echo "$log:"; tail -60 "$log"; done' \
        >&2 2>/dev/null || true
      fail 'the visa test page did not open in the hosted window (see above)'
    fi
    VM_HOST_RUN="sudo -n podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all tilo-linux-vm" \
      VM_HOST_CONTAINER=tilo-sh-vm-host-0 \
      python3 "$here/integration/vm-proofs.py" "$hosted_workspace" "$vm_account" "$work/vm" \
      || {
        echo '--- the VM on its host:' >&2
        vm_host status >&2 2>/dev/null || true
        echo '--- the VM host said:' >&2
        sudo -n podman logs --tail 60 tilo-sh-vm-host-0 >&2 2>/dev/null || true
        echo '--- what the host said of its VMs:' >&2
        sudo -n podman logs tilo-sh-vm-host-0 2>&1 \
          | grep -E 'tilo-linux-vm hosted:' | tail -40 >&2 || true
        echo '--- the server said of the Browser:' >&2
        bash "$here/stack.sh" logs server 2>/dev/null \
          | grep -iE 'browser|environment|hosted|session|suspend|stop' | tail -60 >&2 || true
        fail 'the VM is not what a hosted Browser needs (see above, and vm-proofs.json)'
      }
  fi

  if runs_step 12; then
    step '12. the agent fills, submits and is held in the hosted Browser'
    set +e
    hosted_flow --fill 2>&1 | tee "$work/hosted-fill.log"
    fill_status=${PIPESTATUS[0]}
    set -e
    if [[ $fill_status != 0 ]]; then
      # The person at their shell, in their own browser: what their page said,
      # and what they saw of the hosted window, when the flow stopped.
      echo '--- the person at their shell said:' >&2
      sed -n '1,200p' "$work/hosted/shell.jsonl" >&2 2>/dev/null || true
      tail -40 "$work/hosted/shell.log" >&2 2>/dev/null || true
      fail "the hosted Browser flow failed (see $work/hosted-fill.log)"
    fi
    echo "ok: the hosted Browser ran in $((SECONDS - launch))s, its open taking ${hosted_seconds}s"
  fi

  # The Linux half of a workspace's VM: the commands its people and their
  # agents run there, what the plans file lets run, and the account they run
  # as. They read the workspace the Browser opened above, so that both halves
  # are held to the same VM a customer gets.
  if runs_step 13; then
    step '13. a Linux command in a workspace without a card on file is refused'
    # The provider takes the card off the account for this step: booting a VM
    # and proving what is in it takes the card, and the refusal is what an
    # account without one gets. It is put back before the step ends, so a run
    # that stops here leaves the file as it found it.
    python3 "$here/integration/plans.py" --remove "$vm_account" "$plans" >/dev/null
    refused=0
    hosted_terminal --refused || refused=$?
    python3 "$here/integration/plans.py" "$vm_account" "$plans" >/dev/null
    [[ $refused == 0 ]] || fail "the plans rule did not hold for a Linux command (see $work/hosted/refused.json)"
    echo "ok: a Linux command without a card is refused in $((SECONDS - at))s, saying where a card is added"
  fi

  if runs_step 14; then
    step '14. the provider puts the account on file, and what it refused runs'
    hosted_terminal --card || fail "the card on file did not release what it refused (see $work/hosted/terminal.json)"
    echo "ok: the card on file released the Browser's page action and the account's Linux commands in $((SECONDS - at))s"
  fi

  if runs_step 15; then
    step "15. the workspace's Terminal runs a command in its VM"
    hosted_terminal --terminal || fail "the workspace's Terminal did not run a command in its VM (see $work/hosted/terminal-run.json)"
    echo "ok: the Terminal ran in the VM in $((SECONDS - at))s, recorded as the run it became and in History"
  fi

  if runs_step 16; then
    step "16. an agent's key runs a command in the VM through the tilo command"
    hosted_terminal --agent || fail "the agent's command did not run in the VM (see $work/hosted/terminal.json)"
    echo "ok: a key scoped to the workspace ran a command through the tilo command in $((SECONDS - at))s, and History names it"
  fi

  if runs_step 17; then
    step '17. the Linux session and the Browser session are isolated'
    hosted_terminal --isolated || fail "the tenant's commands and the Browser's session are not held apart (see $work/hosted/isolation.json)"
    echo "ok: the Linux session ran as the tenant in $((SECONDS - at))s, and neither account read the other's files"
  fi

  if runs_step 18; then
    step "18. a Linux GUI app opens in the workspace's VM, on a person's shell"
    # What the workspace's VM takes of this host, as the VM host measures it,
    # before the app and with it open: what admitting a VM counts.
    vm_resident() {
      vm_host status 2>/dev/null | python3 -c '
import re, sys
workspace = sys.argv[1]
for line in sys.stdin.read().splitlines():
    fields = line.split()
    if fields and (fields[0] == workspace or fields[0].startswith(workspace + "/")):
        found = re.search(r"(\d+) MiB", line)
        print(found.group(1) if found else "")
        break' "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["workspace"])' "$work/hosted/open.json" 2>/dev/null)"
    }
    resident_before=$(vm_resident)
    if ! hosted_terminal --gui; then
      echo "the VM on its host: ${resident_before:-?} MiB before the app, $(vm_resident || true) MiB after" >&2
      echo '--- the apps session in the VM said:' >&2
      sudo -n podman exec tilo-sh-vm-host-0 sh -c \
        'find /var /srv /mnt -name apps.log -path "*/vms/*" 2>/dev/null | while read -r log; do echo "$log:"; tail -80 "$log"; done' \
        >&2 2>/dev/null || true
      echo '--- the VM host said:' >&2
      sudo -n podman logs --tail 40 tilo-sh-vm-host-0 >&2 2>/dev/null || true
      fail "the Linux app did not open on the person's shell, or the typing did not take (see $work/hosted/gui.json and $work/hosted/gui-person)"
    fi
    resident_after=$(vm_resident)
    echo "the VM on its host: ${resident_before:-?} MiB before the app, ${resident_after:-?} MiB with it open"
    python3 - "$work/hosted/gui.json" "${resident_before:-}" "${resident_after:-}" <<'EOF_GUI'
import json, sys
path, before, after = sys.argv[1:]
report = json.load(open(path))
report["host_resident_mib"] = {"before": int(before) if before else None, "after": int(after) if after else None}
json.dump(report, open(path, "w"), indent=2)
EOF_GUI
    echo "ok: a Linux app opened in the VM and on the person's shell, and took their typing, in $((SECONDS - at))s"
  fi
fi

if runs_any 2 3 4 5 6; then
  visa_run
fi

if runs_step 7; then
  entry_visits
fi


# The person's page, which answered the hosted steps, can go: its browser
# closes as one a person closes does, after the report it printed first.
if [[ -n ${person_asks:-} ]]; then
  printf 'done\n' > "$person_asks"
fi

# And still none, now that the steps' people signed in, kept keys and connected
# accounts: each of those is an entry of their workspace's vault.
host_keeps_no_vault || fail "the server kept a sealed store of its own beside its workspaces' vaults"

python3 - "$report" "$bring_up" "$visa_seconds" "${TILO_SH_SEED:-0}" "${entries[@]}" <<'EOF'
import json, os, statistics, sys
path, bring_up, visa, seeded, *entries = sys.argv[1:]
entries = [int(ms) for ms in entries]
report = {
    "bring_up_seconds": int(bring_up),
    "visa_run_seconds": int(visa),
    "lounge_seeded": seeded == "1",
    "entry_ms": entries,
    "entry_ms_median": statistics.median(entries) if entries else None,
}
work = os.path.dirname(path)
# The hosted Browser's and its Linux commands' own numbers, when this stack
# ran with its VM host.
for name, key in (("hosted/report.json", "hosted"), ("vm/vm-proofs.json", "vm"),
                  ("hosted/terminal.json", "linux"), ("hosted/gui.json", "linux_app")):
    where = os.path.join(work, name)
    if os.path.isfile(where):
        report[key] = json.load(open(where))
json.dump(report, open(path, "w"), indent=2)
print(json.dumps(report))
EOF
step "all of it held (report: $report)"
