#!/usr/bin/env bash
# A workspace kept by a server of an earlier build is handed over, peer to peer, to a server of
# this commit, with both running: the way a workspace moves to another server without a copy of
# its state. Two tilo.sh stacks come up on this machine from this checkout's stack.sh, the first
# from the earlier build's programs and the second from this commit's, each with its own person.
# Then, each step said as it passes or fails:
#
#   1. the earlier server holds a workspace with members and records in notes, chat, files and
#      mail, counted as its API answers them;
#   2. this commit's server is up, lets the workspace's owner in as one of its owners (which a
#      server asks before it keeps a workspace for someone), and its own person is made an
#      owner of the workspace;
#   3. the workspace's owner has the earlier server hand it over (`workspace/servers#add`, the
#      governance action `server.add`, with this commit's server's site);
#   4. this commit's server holds every record and member, and a write on either server reaches
#      the other while both run;
#   5. the owner has this commit's server let the earlier one go (`server.remove`), the earlier
#      stack stops, and the workspace is still served and still takes writes.
#
# Records are counted from each server's side, through its own API with the owner's key. Both
# stacks keep to loopback: neither is reached through a packet relay of the web.
#
# usage: test-server-handover.sh WORK
#
# WORK keeps both stacks' state (WORK/from, WORK/to) and their logs; what each step read is
# in WORK/steps. From the environment:
#   TILO_HANDOVER_FROM_PROGRAMS  the earlier build's programs, as its own
#                                build-on-builder.sh or tilo-sh-programs.sh lays them out
#                                (tilo-server, tilo-api, tilo, ...)
#   TILO_HANDOVER_FROM_LOUNGE    that build's server context, as its own lounge-context.sh lays
#                                it out
#   TILO_SH_PROGRAMS             this commit's programs (tilo-sh-programs); the stack lays
#                                this commit's server context out itself
#   TILO_SH_FONTS                this commit's product font catalog (lounge-fonts); the
#                                earlier stack takes the one in its server context
#   TILO_SH_CADDY                optional: a Caddy, else the stack fetches its pinned one
#   TILO_HANDOVER_BASE           optional: the earlier stack's port base (26500); this
#                                commit's is 100 above it
set -euo pipefail
usage='usage: test-server-handover.sh WORK'
work=${1:?$usage}
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
say() { printf 'server-handover: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }
from_programs=${TILO_HANDOVER_FROM_PROGRAMS:?TILO_HANDOVER_FROM_PROGRAMS names the programs of the earlier build}
from_lounge=${TILO_HANDOVER_FROM_LOUNGE:?TILO_HANDOVER_FROM_LOUNGE names the server context of the earlier build}
to_programs=${TILO_SH_PROGRAMS:?TILO_SH_PROGRAMS names programs of this commit}
fonts=${TILO_SH_FONTS:?TILO_SH_FONTS names the product font catalog}
for dir in "$from_programs" "$to_programs"; do
  [[ -x $dir/tilo-server && -x $dir/tilo-api && -x $dir/tilo ]] \
    || die "$dir holds no tilo-server, tilo-api and tilo"
done
[[ -x $from_lounge/bin/tilo-server ]] || die "$from_lounge is no laid-out server context"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"
from_base=${TILO_HANDOVER_BASE:-26500}
to_base=$((from_base + 100))
# A test stack is never reached through a relay of the web.
export TILO_SH_REACHED_THROUGH=

mkdir -p "$work"
work=$(cd -- "$work" && pwd)
steps=$work/steps
mkdir -p "$steps" "$work/tmp"
# Each owner's runtime directory is under TMPDIR, and a socket path there must stay short.
export TMPDIR=$work/tmp
data() { python3 -I "$here/server-handover.py" "$@"; }

# One stack's stack.sh, by its side: `from` the earlier build, `to` this commit.
stack() {
  local side=$1
  shift
  if [[ $side == from ]]; then
    TILO_SH_STATE=$work/from TILO_SH_PORT_BASE=$from_base \
      TILO_SH_PROGRAMS=$from_programs TILO_SH_LOUNGE=$from_lounge \
      TILO_SH_TILO_BIN=$from_programs/tilo TILO_SH_SITE=$work/site \
      TILO_SH_REMOTE=0 TILO_SH_VM_HOST=0 TILO_SH_HOME_APP= TILO_SH_ADMISSION=open \
      bash "$here/stack.sh" "$@"
  else
    TILO_SH_STATE=$work/to TILO_SH_PORT_BASE=$to_base \
      TILO_SH_PROGRAMS=$to_programs TILO_SH_LOUNGE= \
      TILO_SH_TILO_BIN=$to_programs/tilo TILO_SH_SITE=$work/site \
      TILO_SH_REMOTE=0 TILO_SH_VM_HOST=0 TILO_SH_HOME_APP= TILO_SH_ADMISSION=open \
      bash "$here/stack.sh" "$@"
  fi
}
# The side's API, as its owner: what `data` talks to.
api() {
  local side=$1
  unset TILO_API_URL TILO_API_KEY
  eval "$(stack "$side" env)"
  export TILO_API_URL TILO_API_KEY
}
cleanup() {
  stack to down --wipe >/dev/null 2>&1 || true
  stack from down --wipe >/dev/null 2>&1 || true
}
trap 'stack to down >/dev/null 2>&1 || true; stack from down >/dev/null 2>&1 || true' EXIT

failed=()
passed=()
pass() { passed+=("$1"); say "PASS $1"; }
fail() { failed+=("$1: $2"); say "FAIL $1: $2"; }
# The end: every step's verdict, and the stacks stopped (their state kept, for the logs).
report() {
  say "passed: ${#passed[@]}, failed: ${#failed[@]}"
  local line
  for line in "${failed[@]}"; do say "  failed $line"; done
  ((${#failed[@]} == 0))
}

# -- fresh states -----------------------------------------------------------------------------
cleanup
rm -rf "$work/from" "$work/to" "$work/from-keys" "$work/to-keys" "$work/site" "$steps"/*
mkdir -p "$work/site/site" "$work/from/native" "$work/to/native"
printf '<!doctype html><title>tilo.sh</title>\n' > "$work/site/site/index.html"
cp "$repo/dist/install/tilo.sh" "$work/site/install.sh"
cp -r "$from_lounge/fonts" "$work/from/native/fonts"
cp -r "$fonts" "$work/to/native/fonts"
from_origin=http://127.0.0.1:$from_base
to_origin=http://127.0.0.1:$to_base

# -- 1. the earlier server, with a workspace ---------------------------------------------------
say "1. the earlier build's stack on $from_base ($from_programs)"
if ! stack from up >"$steps/from-up.log" 2>&1; then
  fail 'the earlier stack comes up' "$(tail -5 "$steps/from-up.log")"
  report; exit 1
fi
stack from owner --here >/dev/null
api from
if ! data fill 'Handover check' >"$steps/written.json" 2>"$steps/fill.err"; then
  fail 'the earlier server takes the records' "$(cat "$steps/fill.err")"
  report; exit 1
fi
workspace=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["workspace"])' "$steps/written.json")
# Its members: two more people, made the way a person's tilo makes one, at two roles.
for role in editor viewer; do
  home=$work/people/$role
  mkdir -p "$home"
  made=$(HOME=$home XDG_CONFIG_HOME=$home/.config XDG_STATE_HOME=$home/.local/state \
    XDG_DATA_HOME=$home/.local/share XDG_CACHE_HOME=$home/.cache TILO_IDENTITY_ROOT=$home/.local/share/tilo \
    "$from_programs/tilo" login --new-identity --json 2>"$steps/person-$role.err") \
    || { fail "a $role is made" "$(cat "$steps/person-$role.err")"; continue; }
  key=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["user_public_key"])' "$made")
  stack from tilo workspace people add "$key" --role "$role" -w "$workspace" \
    >"$steps/member-$role.log" 2>&1 \
    || fail "the earlier server takes a $role" "$(tail -3 "$steps/member-$role.log")"
done
data counts "$workspace" >"$steps/from-counts.json"
say "the earlier server holds: $(cat "$steps/from-counts.json")"
if data expect "$workspace" "$steps/written.json" 60 >/dev/null 2>"$steps/from-expect.err"; then
  pass '1. the earlier server holds the workspace and its records'
else
  fail '1. the earlier server holds the workspace and its records' "$(cat "$steps/from-expect.err")"
fi

# -- 2. this commit's server, the workspace's owner let in ------------------------------------------
# Each stack has its own person. The workspace's owner is let in on this commit's server as one
# of its owners, which a server asks before it keeps a workspace for someone; and this commit's
# stack's person is made an owner of the workspace, who later lets the earlier server go from
# this commit's side.
say "2. this commit's stack on $to_base ($to_programs)"
if ! stack to up >"$steps/to-up.log" 2>&1; then
  fail '2. this commit'\''s stack comes up' "$(tail -5 "$steps/to-up.log")"
  report; exit 1
fi
if ! stack to owner --here >"$steps/to-owner.log" 2>&1; then
  fail '2. this commit'\''s stack names its owner' "$(tail -3 "$steps/to-owner.log")"
  report; exit 1
fi
if ! stack to owner "$(cat "$work/from/owner/key")" >"$steps/to-lets-in.log" 2>&1; then
  fail '2. this commit'\''s server lets the workspace'\''s owner in' "$(tail -3 "$steps/to-lets-in.log")"
  report; exit 1
fi
if stack from tilo workspace people add "$(cat "$work/to/owner/key")" --role owner -w "$workspace" \
  >"$steps/member-to-owner.log" 2>&1; then
  pass '2. this commit'\''s stack is up, each owner let in on the other'
else
  fail '2. the earlier server makes this commit'\''s owner an owner of the workspace' "$(tail -3 "$steps/member-to-owner.log")"
fi
api from
data counts "$workspace" >"$steps/from-counts.json"

# -- 3. the hand-over ----------------------------------------------------------------------------
say "3. the earlier server hands the workspace to $to_origin (server.add)"
api from
if data act "$workspace" 'workspace/servers#add' "{\"origin\":\"$to_origin\"}" \
  >"$steps/server-add.json" 2>"$steps/server-add.err"; then
  pass '3. server.add on the earlier server'
  say "it answered: $(cat "$steps/server-add.json")"
else
  fail '3. server.add on the earlier server' "$(cat "$steps/server-add.err")"
fi

# -- 4. both hold it, and each one's writes reach the other ---------------------------------------
say "4. this commit's server holds every record, and writes cross both ways"
api to
listed=0
for _ in $(seq 60); do
  if data workspaces 2>/dev/null | grep -q "\"$workspace\""; then listed=1; break; fi
  sleep 3
done
if ((listed)); then
  pass '4a. this commit'\''s server lists the workspace'
else
  fail '4a. this commit'\''s server lists the workspace' "$(data workspaces 2>&1 | head -c 600)"
fi
if data expect "$workspace" "$steps/written.json" 180 >/dev/null 2>"$steps/to-expect.err"; then
  pass '4b. this commit'\''s server holds every record the earlier one was written'
else
  fail '4b. this commit'\''s server holds every record the earlier one was written' "$(cat "$steps/to-expect.err")"
fi
data counts "$workspace" >"$steps/to-counts.json" 2>&1 || true
say "this commit's server holds: $(cat "$steps/to-counts.json")"
python3 -I - "$steps/from-counts.json" "$steps/to-counts.json" >"$steps/members.err" <<'PY' \
  && pass '4c. this commit'\''s server holds every member at their role' \
  || fail '4c. this commit'\''s server holds every member at their role' "$(cat "$steps/members.err")"
import json, sys
before, after = (json.load(open(path)) for path in sys.argv[1:])
if before.get("members") != after.get("members"):
    print(f"members before: {before.get('members')}, after: {after.get('members')}")
    sys.exit(1)
PY
api from
data write "$workspace" earlier >"$steps/written-from.json" 2>"$steps/write-from.err" \
  || fail '4d. the earlier server takes a write' "$(cat "$steps/write-from.err")"
api to
data write "$workspace" newer >"$steps/written-to.json" 2>"$steps/write-to.err" \
  || fail '4e. this commit'\''s server takes a write' "$(cat "$steps/write-to.err")"
if [[ -s $steps/written-from.json ]]; then
  if data expect "$workspace" "$steps/written-from.json" 120 >/dev/null 2>"$steps/from-to.err"; then
    pass '4d. a write on the earlier server reaches this commit'\''s'
  else
    fail '4d. a write on the earlier server reaches this commit'\''s' "$(cat "$steps/from-to.err")"
  fi
fi
if [[ -s $steps/written-to.json ]]; then
  api from
  if data expect "$workspace" "$steps/written-to.json" 120 >/dev/null 2>"$steps/to-from.err"; then
    pass '4e. a write on this commit'\''s server reaches the earlier one'
  else
    fail '4e. a write on this commit'\''s server reaches the earlier one' "$(cat "$steps/to-from.err")"
  fi
fi

# -- 5. the earlier server let go -------------------------------------------------------------------
say "5. this commit's server lets the earlier one go (server.remove), and the earlier stack stops"
api to
data act "$workspace" 'workspace/servers#list' >"$steps/servers-before-remove.json" 2>&1 || true
if data act "$workspace" 'workspace/servers#remove' "{\"server\":\"$from_origin\"}" \
  >"$steps/server-remove.json" 2>"$steps/server-remove.err"; then
  pass '5a. server.remove on this commit'\''s server'
  say "it answered: $(cat "$steps/server-remove.json")"
else
  fail '5a. server.remove on this commit'\''s server' "$(cat "$steps/server-remove.err")"
fi
stack from down >"$steps/from-down.log" 2>&1 || true
data write "$workspace" remaining >"$steps/written-after.json" 2>"$steps/write-after.err" \
  || fail '5b. this commit'\''s server takes a write alone' "$(cat "$steps/write-after.err")"
cat "$steps/written.json" "$steps/written-from.json" "$steps/written-to.json" "$steps/written-after.json" 2>/dev/null \
  | python3 -I -c '
import json, sys
merged = {}
for line in sys.stdin:
    if line.strip():
        for key, value in json.loads(line).items():
            merged[key] = value if key == "workspace" else merged.get(key, []) + value
print(json.dumps(merged))' >"$steps/everything.json"
if data expect "$workspace" "$steps/everything.json" 60 >/dev/null 2>"$steps/after.err"; then
  pass '5b. the workspace is still served, every record in it, with the earlier server gone'
else
  fail '5b. the workspace is still served, every record in it, with the earlier server gone' "$(cat "$steps/after.err")"
fi
data counts "$workspace" >"$steps/after-counts.json" 2>&1 || true
say "this commit's server holds, alone: $(cat "$steps/after-counts.json")"
report
