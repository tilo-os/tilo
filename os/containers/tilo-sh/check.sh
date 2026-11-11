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
# With TILO_SH_VM_HOST=1 it is the same stack with its VM host in it, and the
# end of the check is the Browser of a hosted workspace (docs/hosted-browser.md),
# in the workspace's own VM:
#
#   8. the visa test page (apps/browser/test-pages/visa-form.html), served
#      where a VM's network may reach it: the relay endpoint's own address and
#      port, the one destination besides the internet a VM is given;
#   9. the guest this stack's VM host boots carries the Browser there: the
#      headless session and the image's unprivileged browser user;
#  10. a key opens Browser in a hosted workspace, timed to a live window;
#  11. the VM it runs in: every renderer sandboxed, the session running as the
#      image's browser user, its device key, and what the page costs in memory
#      (integration/vm-proofs.py);
#  12. the agent fills the visa test page through page actions, captures it,
#      stops before submitting; a reviewer who joined by invitation reads its
#      run in History, and page actions past the plans file's free ones are
#      refused until the provider adds the account there.
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
# Playwright's), and uv (or python3 with httpx) for the Python SDK. With
# TILO_SH_VM_HOST=1 it also needs /dev/kvm and root through sudo, and the
# workspace guest image must carry the headless browser session.
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

# The reviewer's helper, up for every run that invites someone: each
# invitation posted to it is another person in their own headless Chromium.
reviewer_port=$((base + 50))
sdk_python "$here/integration/reviewer.py" \
  "$reviewer_port" "$site" "$relay" "$door" "$tilo" "$work" > "$work/reviewer.out" 2> "$work/reviewer.log" &
pids+=($!)
for _ in $(seq 100); do grep -q invitee "$work/reviewer.out" 2>/dev/null && break; sleep 0.2; done
grep -q invitee "$work/reviewer.out" || fail "the reviewer did not start: $(cat "$work/reviewer.log")"

# The hosted Browser's checks (8-11) run here, on that key: the visa run below
# ends by revoking it.
hosted=${TILO_SH_VM_HOST:-0}
hosted_report=$work/hosted/report.json
if [[ $hosted == 1 ]]; then
  [[ ${TILO_SH_IMAGES:-} == 1 ]] || fail 'TILO_SH_VM_HOST=1 runs the stack as its images: the VM host is one'
  [[ -e /dev/kvm ]] || fail '/dev/kvm: the hosted Browser runs in a VM, and this machine has none'
  vm_account=$(vm_host env 2>/dev/null | sed -n 's/^TILO_HOSTED_ACCOUNT=//p')
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

  step '10. a key opens Browser in a hosted workspace, in its VM'
  hosted_run() { # PHASE
    TILO_INVITEE=http://127.0.0.1:$reviewer_port/accept \
      HOSTED_VISA_PLANS=$plans HOSTED_VISA_ACCOUNT=$vm_account \
      sdk_python "$here/integration/hosted-visa.py" "$door" "$key" "$work/hosted" "$form_url" "$1"
  }
  launch=$SECONDS
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

  step '11. the VM the Browser ran in: its sandbox, its user, its memory'
  VM_HOST_RUN="sudo -n podman exec tilo-sh-vm-host-0 setpriv --reuid=10001 --regid=10001 --init-groups --inh-caps=-all tilo-legacy" \
    VM_HOST_CONTAINER=tilo-sh-vm-host-0 \
    python3 "$here/integration/vm-proofs.py" "$hosted_workspace" "$vm_run_account" "$work/vm" \
    || fail 'the VM is not what a hosted Browser needs (see above, and vm-proofs.json)'

  step '12. the agent fills the visa form in the hosted Browser'
  set +e
  hosted_run --fill 2>&1 | tee "$work/hosted-fill.log"
  fill_status=${PIPESTATUS[0]}
  set -e
  [[ $fill_status == 0 ]] || fail "the hosted Browser flow failed (see $work/hosted-fill.log)"
  echo "ok: the hosted Browser ran in $((SECONDS - launch))s, its open taking ${hosted_seconds}s"
fi

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

step '7. a fresh visitor enters the Lounge, timed'
visits=${TILO_SH_ENTRY_VISITS:-3}
entries=()
for visit in $(seq "$visits"); do
  node "$here/integration/person.mjs" "$site" "$relay" "$work/visitor-$visit" > "$work/visitor-$visit.json" \
    || fail "visitor $visit did not get in (see $work/visitor-$visit)"
  entries+=("$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["joinedMs"])' "$work/visitor-$visit.json")")
done

python3 - "$report" "$bring_up" "$visa_seconds" "${TILO_SH_SEED:-0}" "${entries[@]}" <<'EOF'
import json, os, statistics, sys
path, bring_up, visa, seeded, *entries = sys.argv[1:]
entries = [int(ms) for ms in entries]
report = {
    "bring_up_seconds": int(bring_up),
    "visa_run_seconds": int(visa),
    "lounge_seeded": seeded == "1",
    "entry_ms": entries,
    "entry_ms_median": statistics.median(entries),
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
