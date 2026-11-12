#!/usr/bin/env bash
# The local tilo.sh, brought up and used end to end (stack.sh check):
#
#   1. a fresh startup identity, in its own headless Chromium on the local
#      site, approves a server key `tilo keys create` asks for, on the site's
#      own /sign-in page; the pictures of what it reads there (the sign-in
#      card, the door's grants page, and the key that page makes) are kept in
#      WORK/startup, light and dark. The page also makes a recovery key for
#      them, so the machine can hold that person and answer what their hosts
#      ask them (`tilo-approve`);
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
#   8. the visa test page (apps/browser/test-pages/visa-form.html), served
#      where a VM's network may reach it: the relay endpoint's own address and
#      port, the one destination besides the internet a VM is given;
#   9. the guest this stack's VM host boots carries the Browser there: the
#      headless session and the image's unprivileged browser user;
#  9b. the host warms the template that size's VMs start from, whose first boot
#      is cold by necessity;
#  10. a key opens Browser in a hosted workspace, timed to a live window, in a
#      VM that woke as a clone of that template (the host's own measure of the
#      wake is printed), never a cold boot of the guest image;
#  11. the VM it runs in: every renderer sandboxed, the session running as the
#      image's browser user, its device key, and what the page costs in memory
#      (integration/vm-proofs.py);
#  12. the agent fills the visa test page through page actions with the
#      person asked nothing (their key operates web pages), captures it,
#      submits it once the person allows that one submission, is refused when
#      they deny another, and has a click on the submit button held and asked
#      once; a key minted with the scope unticked is asked about the page
#      actions that act on the page; a reviewer who joined by invitation reads
#      the run in
#      History, and page actions past the plans file's free ones are refused
#      until the provider adds the account there.
#
# usage: check.sh [WORK]
# The stack runs this checkout's programs natively (the outputs a check hands
# it: TILO_SH_PROGRAMS, TILO_SH_LOUNGE, TILO_SH_SITE, TILO_SH_CLI_DIR,
# TILO_SH_CADDY, or this machine's own build). WORK keeps what it found
# (pages' consoles and screenshots, the visa run's
# log, report.json); default $TILO_SH_STATE/check. The stack is left up; it
# is stopped first when TILO_SH_FRESH=1 (a new Lounge and door). The tilo
# command is TILO_SH_TILO_BIN, else this checkout's release build the stack
# serves at /releases, else target/{debug,release}/tilo. VISA_APPLICANTS
# sets how many workspaces the run makes (25 here). The questions put to the
# startup person are answered by TILO_SH_TILO_APPROVE (tilo-approve, built by
# the t check beside tilo-enroll), from the root of theirs kept in
# WORK/startup/root, through the relay address the VM host names.
# TILO_SH_ONLY names the steps to run (`1`, `2-6`, `7`, `10`, comma separated)
# to chase one of them without the rest; the stack is brought up either way and
# each step's own seconds are printed beside it.
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
work=${1:-$state/check}
mkdir -p "$work"
find "$work" -mindepth 1 -delete
site=https://tilo.localhost:$((base + 20))
relay=https://127.0.0.1:$base
door=http://localhost:$((base + 10))
began=$SECONDS
at=$SECONDS
step() {
  printf '\n== %s (%ss, %ss in)\n' "$*" $((SECONDS - at)) $((SECONDS - began))
  at=$SECONDS
}
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
# TILO_SH_ONLY names the steps to run, comma separated (`1`, `2-6`, `7`): what
# a step that failed is chased with, instead of the whole check around it.
# Naming none runs them all. A step that reads what an earlier one made is run
# with that one: the visa run (2-6) and the hosted Browser (8-12) read the key
# and the person the startup made in 1, so naming either runs 1 as well. The
# stack is brought up either way.
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
report=$work/report.json
pids=()
trap 'for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done' EXIT

step 'bring the stack up'
up_args=()
[[ ${TILO_SH_FRESH:-} == 1 ]] && up_args+=(--fresh)
[[ ${TILO_SH_SEED:-} == 1 ]] && up_args+=(--seed)
bash "$here/stack.sh" up "${up_args[@]}" || fail 'the stack did not come up'
bring_up=$(cat "$state/bring-up-seconds")

# The stack's addresses and helpers (find_tilo, sdk_python).
# shellcheck source=stack.sh
. "$here/stack.sh"
tilo=$(find_tilo)
echo "the tilo command: $tilo"

key= plain_key= reviewer_port=
if runs_any 1 2 3 4 5 6 8 9 10 11 12; then
  step '1. a fresh startup identity approves a server key on the local site'
  node "$here/integration/person.mjs" "$site" "$relay" "$work/startup" \
    --key 'Visa backend' --tilo "$tilo" --door "$door" --no-pages \
    --root "$work/startup/person.json" > "$work/startup.json" \
    || fail "the startup's key was not approved (see $work/startup)"
  key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["key"])' "$work/startup.json")
  # The key they did not let operate web pages, made on the door's grants page:
  # what the last step holds to being asked about each page action.
  plain_key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["noPagesKey"])' "$work/startup.json")
  [[ -n $plain_key ]] || fail 'the startup made no key without the Browser scope (see $work/startup)'
  echo "ok: the startup holds its server key, and one that may not operate web pages"
fi

# The reviewer's helper, up for every run that invites someone: each
# invitation posted to it is another person in their own headless Chromium.
if runs_any 2 3 4 5 6 12; then
  reviewer_port=$((base + 50))
  sdk_python "$here/integration/reviewer.py" \
    "$reviewer_port" "$site" "$relay" "$door" "$tilo" "$work" > "$work/reviewer.out" 2> "$work/reviewer.log" &
  pids+=($!)
  for _ in $(seq 100); do grep -q invitee "$work/reviewer.out" 2>/dev/null && break; sleep 0.2; done
  grep -q invitee "$work/reviewer.out" || fail "the reviewer did not start: $(cat "$work/reviewer.log")"
fi

# The hosted Browser's checks (8-11) run here, on that key: the visa run below
# ends by revoking it.
hosted=${TILO_SH_VM_HOST:-0}
hosted_report=$work/hosted/report.json
if [[ $hosted == 1 ]]; then
  [[ -e /dev/kvm ]] || fail '/dev/kvm: the hosted Browser runs in a VM, and this machine has none'
  # What the relay reads to place here: the account a card is on file for,
  # and the address a VM's browser session reaches the relay at.
  vm_env=$(vm_host env 2>/dev/null) || true
  vm_account=$(sed -n 's/^TILO_HOSTED_ACCOUNT=//p' <<<"$vm_env")
  relay_address=$(sed -n 's/^TILO_HOSTED_BROWSER_RELAY=//p' <<<"$vm_env")
  [[ -n $vm_account ]] || fail 'the VM host is not up, so the relay places no Browser: stack.sh status says why'
  plans=$state/secrets/vm-host-plans.json
  # The plans file the provider would edit is the VM host's, made as root: the
  # check plays the provider, adding the account to it mid-run.
  [[ -w $plans ]] || sudo -n chown "$(id -u):$(id -g)" "$plans" 2>/dev/null \
    || fail "the plans file $plans is not the check's to edit"
  # A Linux program in a VM is admitted for an account with a card on file,
  # which is what the proofs and the guest check below run as.
  vm_run_account=$(python3 -c 'import json,sys; print(next(iter(json.load(open(sys.argv[1])).get("accounts") or {})))' "$plans") \
    || fail "the plans file $plans names no account with a card"
  # The person's approving device: the root the page made for them, opened
  # here, so what the relay asks them is answered and printed.
  approve=${TILO_SH_TILO_APPROVE:-tilo-approve}
  command -v "$approve" >/dev/null 2>&1 || [[ -x $approve ]] \
    || fail "no tilo-approve: TILO_SH_TILO_APPROVE names the program the t check builds"
  "$approve" person --person "$work/startup/person.json" --root "$work/startup/root" \
    > "$work/startup/person-root.json" \
    || fail "the startup person could not be held from the recovery key their page made (see $work/startup)"
  [[ -n $relay_address ]] || fail 'the VM host names no relay address for Browser sessions'

  if runs_any 8 10 12; then
    step '8. the visa test page, where a VM may reach it'
    form_dir=$work/form
    mkdir -p "$form_dir"
    cp "$repo/apps/browser/test-pages/visa-form.html" "$form_dir/visa-form.html"
    # A VM reaches the public internet and one address of this machine: the
    # relay's own endpoint, as the VM dials it (10.0.2.4, which pasta maps to
    # this machine's loopback). The relay's endpoint is UDP; its port's TCP side
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
    guest_says=$(sudo -n podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 \
        --init-groups --inh-caps=-all tilo-legacy hosted run --run /run/tilo-hosted \
        --workspace ws-browser-guest --account "$vm_run_account" -- /bin/sh -c \
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
    deadline=$((SECONDS + 180))
    while ((SECONDS < deadline)); do
      template=$(sudo -n podman logs tilo-sh-vm-host-0 2>&1 | sed -n 's/.*template is ready$/ready/p' | head -1)
      [[ -z $template ]] || break
      sleep 1
    done
    [[ -n $template ]] \
      || fail "the VM host made no warm template in 180 s, so every workspace's VM boots cold: what it said is in sudo -n podman logs tilo-sh-vm-host-0 (a template is counted at the size's whole memory, and TILO_SH_VM_HOST_MEMORY_MIB is the room the host may use)"
    echo "ok: the template was ready in $((SECONDS - warmed))s"
  fi

  hosted_workspace= hosted_seconds=0
  hosted_run() { # PHASE
    TILO_INVITEE=http://127.0.0.1:$reviewer_port/accept \
      HOSTED_VISA_PLANS=$plans HOSTED_VISA_ACCOUNT=$vm_account \
      HOSTED_VISA_APPROVER=$approve HOSTED_VISA_ROOT=$work/startup/root \
      HOSTED_VISA_RELAY=$relay_address HOSTED_VISA_UNSCOPED_KEY=$plain_key \
      sdk_python "$here/integration/hosted-visa.py" "$door" "$key" "$work/hosted" "$form_url" "$1"
  }
  launch=$SECONDS
  if runs_step 10; then
    step '10. a key opens Browser in a hosted workspace, in its VM'
    set +e
    hosted_run --open 2>&1 | tee "$work/hosted-open.log"
    hosted_status=${PIPESTATUS[0]}
    set -e
    hosted_seconds=$((SECONDS - launch))
    if [[ $hosted_status != 0 ]]; then
      echo '--- the VM host said:' >&2
      sudo -n podman logs --tail 80 tilo-sh-vm-host-0 >&2 2>/dev/null || true
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
    VM_HOST_RUN="sudo -n podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all tilo-legacy" \
      VM_HOST_CONTAINER=tilo-sh-vm-host-0 \
      python3 "$here/integration/vm-proofs.py" "$hosted_workspace" "$vm_run_account" "$work/vm" \
      || fail 'the VM is not what a hosted Browser needs (see above, and vm-proofs.json)'
  fi

  if runs_step 12; then
    step '12. the agent fills, submits and is held in the hosted Browser'
    set +e
    hosted_run --fill 2>&1 | tee "$work/hosted-fill.log"
    fill_status=${PIPESTATUS[0]}
    set -e
    [[ $fill_status == 0 ]] || fail "the hosted Browser flow failed (see $work/hosted-fill.log)"
    echo "ok: the hosted Browser ran in $((SECONDS - launch))s, its open taking ${hosted_seconds}s"
  fi
fi

visa_seconds=0
entries=()
if runs_any 2 3 4 5 6; then
  step '2-6. the visa run against the local door'
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
fi

if runs_step 7; then
  step '7. a fresh visitor enters the Lounge, timed'
  visits=${TILO_SH_ENTRY_VISITS:-3}
  for visit in $(seq "$visits"); do
    node "$here/integration/person.mjs" "$site" "$relay" "$work/visitor-$visit" > "$work/visitor-$visit.json" \
      || fail "visitor $visit did not get in (see $work/visitor-$visit)"
    entries+=("$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["joinedMs"])' "$work/visitor-$visit.json")")
  done
fi

step '7b. a visitor keeps themselves with a passkey and signs in with it again'
# The site's name is the passkey's relying party, as tilo.sh is on tilo.sh: a
# browser registers none for an address, which the local site was once.
node "$here/integration/person.mjs" "$site" "$relay" "$work/passkey" --passkey > "$work/passkey.json" \
  || fail "the passkey did not register or sign in (see $work/passkey)"

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
# The hosted Browser's own numbers, when this stack ran with its VM host.
for name, key in (("hosted/report.json", "hosted"), ("vm/vm-proofs.json", "vm")):
    where = os.path.join(work, name)
    if os.path.isfile(where):
        report[key] = json.load(open(where))
json.dump(report, open(path, "w"), indent=2)
print(json.dumps(report))
EOF
step "all of it held (report: $report)"
