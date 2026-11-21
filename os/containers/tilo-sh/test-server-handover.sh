#!/usr/bin/env bash
# A workspace kept by a server of an earlier build is handed over, peer to peer, to a server of
# this commit: the way a workspace moves to another server without a copy of its state. The
# earlier server serves only until this commit's is current, then stops, and this commit's
# finishes the move alone. Two tilo.sh stacks come up on this machine from this checkout's stack.sh, the first
# from the earlier build's programs and the second from this commit's. The second is another
# server of the same person, as a real move's is: it starts with the earlier stack's workspace
# root (the operator's secret), its Lounge's id and its person, and a fresh state. Then, each
# step said as it passes or fails:
#
#   1. the earlier server holds a workspace with members and records in notes, chat, files and
#      mail, counted as its API answers them;
#   2. this commit's server founds nothing under the Lounge's id: it waits until the earlier
#      server hands it the Lounge (`workspace/servers#add`), then comes up. Its person signs in
#      on a computer of their own with their recovery words (`tilo login --recovery-key`), as
#      every device an earlier tilo kept a person on does;
#   3. the workspace's owner has the earlier server hand it over (`workspace/servers#add`, the
#      governance action `server.add`, with this commit's server's site);
#   4. this commit's server holds every record and member, and a write on the earlier server
#      reaches it. The earlier build kept the workspace for its person while its server owned
#      it;
#   5. the earlier stack stops, its state kept, as a server taken off the move does; the owner
#      has this commit's server let it go (`server.remove`, naming it by its device key, since
#      its site answers no more), and the workspace is still served, takes writes, and its
#      owner's tilo reads through this commit's server what the earlier one was written, and
#      it serves the bytes of a file the earlier one kept.
#      Choosing its servers, the person it was kept for is its owner from then on. Nothing
#      waits on the earlier server: the owner's key decides where the workspace is kept, and the
#      next of its holders makes its base once the earlier one is gone.
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

# -- 2. this commit's server, another of the same person's -----------------------------------
# It takes the earlier stack's workspace root and the Lounge's id, as a server a workspace moves
# to is given its operator's secret, and comes up only once it holds the Lounge: the earlier
# server's owner hands it over as any workspace is handed to another server. Its person is the
# earlier stack's, signed in on a computer of their own with their recovery words, as every
# device an earlier tilo kept a person on signs in again: it approves a key of its own for this
# commit's API and later lets the earlier server go from this commit's side.
say "2. this commit's stack on $to_base ($to_programs), handed the Lounge"
mkdir -p "$work/to/secrets" "$work/to/server/lounge" "$work/to/owner"
cp "$work/from/secrets/workspace-root-lounge" "$work/to/secrets/workspace-root-lounge"
cp "$work/from/server/lounge/workspace-id" "$work/to/server/lounge/workspace-id"
# The stack's person is the earlier stack's (stack.sh reads them from owner/key).
cp "$work/from/owner/key" "$work/to/owner/key"
lounge=$(cat "$work/from/server/lounge/workspace-id")
stack to up >"$steps/to-up.log" 2>&1 &
to_up=$!
handed=0
for _ in $(seq 120); do
  kill -0 "$to_up" 2>/dev/null || break
  if data act "$lounge" 'workspace/servers#add' "{\"origin\":\"$to_origin\"}" \
    >"$steps/lounge-add.json" 2>"$steps/lounge-add.err"; then
    handed=1
    break
  fi
  sleep 3
done
if ! wait "$to_up"; then
  fail '2. this commit'\''s stack comes up, handed the Lounge' \
    "$(tail -5 "$steps/to-up.log"; ((handed)) || cat "$steps/lounge-add.err")"
  report; exit 1
fi
# The words of the root the earlier tilo kept as a file, typed into this commit's tilo.
if ! data words "$work/from/owner/home/.local/state/tilo/identity/user-identity/privkey.bin" \
  | stack to person login --recovery-key >"$steps/to-login.log" 2>&1; then
  fail '2. the person signs in to this commit'\''s stack with their recovery words' \
    "$(tail -3 "$steps/to-login.log")"
  report; exit 1
fi
if ! stack to owner --here >"$steps/to-owner.log" 2>&1; then
  fail '2. this commit'\''s stack names its owner' "$(tail -3 "$steps/to-owner.log")"
  report; exit 1
fi
pass '2. this commit'\''s stack is up, handed the Lounge'

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

# -- 4. both hold it, and the earlier one's writes reach this commit's ---------------------------
say "4. this commit's server holds every record, and a write on the earlier one reaches it"
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
# The members at their roles, as `before` names them, on this commit's server as `after` does:
# the earlier build names the person it keeps the workspace for an editor beside its server, the
# owner, and this commit names them so until the person chooses the workspace's servers, its
# owner from then on. `owned` asks for the second.
members() {
  python3 -I - "$@" <<'PY'
import json, sys
before, after = (json.load(open(path)) for path in sys.argv[1:3])
owned = sys.argv[3:] == ["owned"]
had = dict(before.get("members") or {})
handed = dict(had)
if isinstance(before.get("members"), dict) and "owner" not in handed and handed.get("editor"):
    handed["editor"] -= 1
    handed["owner"] = 1
    handed = {role: count for role, count in handed.items() if count}
wanted = [handed] if owned else [had, handed]
if after.get("members") not in wanted:
    print(f"members before: {had}, wanted: {' or '.join(map(str, wanted))}, after: {after.get('members')}")
    sys.exit(1)
PY
}
members "$steps/from-counts.json" "$steps/to-counts.json" >"$steps/members.err" \
  && pass '4c. this commit'\''s server holds every member at their role' \
  || fail '4c. this commit'\''s server holds every member at their role' "$(cat "$steps/members.err")"
api from
data write "$workspace" earlier >"$steps/written-from.json" 2>"$steps/write-from.err" \
  || fail '4d. the earlier server takes a write' "$(cat "$steps/write-from.err")"
if [[ -s $steps/written-from.json ]]; then
  api to
  if data expect "$workspace" "$steps/written-from.json" 120 >/dev/null 2>"$steps/from-to.err"; then
    pass '4d. a write on the earlier server reaches this commit'\''s'
  else
    fail '4d. a write on the earlier server reaches this commit'\''s' "$(cat "$steps/from-to.err")"
  fi
fi

# -- 5. the earlier server stops, and this commit's lets it go -------------------------------------
# The earlier server's device key, as it named itself among the holders when it handed the
# workspace over: its site answers no more once it stops.
from_device=$(python3 -I -c '
import json, sys
servers = json.load(open(sys.argv[1]))["result"]["servers"]
print(next(server["device"] for server in servers if server.get("here")))' "$steps/server-add.json" 2>/dev/null || true)
say "5. the earlier stack stops (${from_device:-its device unknown}), and this commit's server lets it go (server.remove)"
stack from down >"$steps/from-down.log" 2>&1 || true
api to
data act "$workspace" 'workspace/servers#list' >"$steps/servers-before-remove.json" 2>&1 || true
if [[ -n $from_device ]] && data act "$workspace" 'workspace/servers#remove' "{\"server\":\"$from_device\"}" \
  >"$steps/server-remove.json" 2>"$steps/server-remove.err"; then
  pass '5a. server.remove on this commit'\''s server, the earlier one stopped'
  say "it answered: $(cat "$steps/server-remove.json")"
else
  fail '5a. server.remove on this commit'\''s server, the earlier one stopped' \
    "$(cat "$steps/server-remove.err" 2>/dev/null || echo 'the earlier server named no device of its own')"
fi
data write "$workspace" remaining >"$steps/written-after.json" 2>"$steps/write-after.err" \
  || fail '5b. this commit'\''s server takes a write alone' "$(cat "$steps/write-after.err")"
cat "$steps/written.json" "$steps/written-from.json" "$steps/written-after.json" 2>/dev/null \
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
# A file's bytes, not only its name: the earlier server kept them, and it answers no more.
wanted=$(python3 -I -c 'import hashlib; print(hashlib.sha256(b"File 1, written before the handover.\n" * 50).hexdigest())')
if data bytes "$workspace" files/docs/handover-1.txt >"$steps/bytes.json" 2>"$steps/bytes.err" \
  && grep -qF "$wanted" "$steps/bytes.json"; then
  pass '5e. this commit'\''s server serves the bytes of a file the earlier one was written'
else
  fail '5e. this commit'\''s server serves the bytes of a file the earlier one was written' \
    "$(cat "$steps/bytes.json" "$steps/bytes.err" 2>/dev/null | head -c 600)"
fi
data counts "$workspace" >"$steps/after-counts.json" 2>&1 || true
say "this commit's server holds, alone: $(cat "$steps/after-counts.json")"
members "$steps/from-counts.json" "$steps/after-counts.json" owned >"$steps/owned.err" \
  && pass '5d. the person the earlier server kept the workspace for owns it, having chosen its servers' \
  || fail '5d. the person the earlier server kept the workspace for owns it, having chosen its servers' \
    "$(cat "$steps/owned.err")"
# The owner's own tilo, against this commit's server alone, reads a message the earlier one was
# written before the hand-over.
earliest=$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["chat/Messages"][0])' "$steps/written.json")
if stack to tilo read chat/Messages -w "$workspace" >"$steps/owner-read.txt" 2>&1 \
  && grep -qF "$earliest" "$steps/owner-read.txt"; then
  pass '5c. the owner'\''s tilo reads, through this commit'\''s server, what the earlier one was written'
else
  fail '5c. the owner'\''s tilo reads, through this commit'\''s server, what the earlier one was written' \
    "$(head -c 600 "$steps/owner-read.txt")"
fi
report
