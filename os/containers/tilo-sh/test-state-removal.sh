#!/usr/bin/env bash
# `down --wipe` removes the stack's own state directory and nothing else: a TILO_SH_STATE that
# names a directory no stack made, or one that holds other stacks' states, is refused and kept
# whole, so a mistyped or parent path never takes what lives beside the stack with it.
#
# No stack starts here: the removal is asked of directories this test lays out. Held:
#
#   - a stack's own state (its `mode` written, nothing of another stack's in it) is removed;
#   - a directory with no stack's `mode`, such as the parent several stacks live in, is refused,
#     and everything in it is still there;
#   - a stack's state that holds another stack's state is refused, and both are still there.
#
# usage: test-state-removal.sh
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/tilo-sh-state-removal.XXXXXX")
trap 'rm -rf "$work"' EXIT
faults=0
fault() {
  echo "FAIL: $*" >&2
  faults=$((faults + 1))
}

export TILO_SH_STATE=$work/sourced TILO_SH_REMOTE=0
unset TILO_SH_LOUNGE
# shellcheck source=stack.sh
. "$here/stack.sh"

# A stack's state as `up` leaves it: its mode, its logs.
lay_out() {
  mkdir -p "$1/logs"
  printf '%s\n' "$2" > "$1/mode"
  printf 'a line\n' > "$1/logs/up.log"
}

# -- the stack's own state -----------------------------------------------------

lay_out "$work/own" remote
state=$work/own
remove_state > "$work/own.said" 2>&1 || fault "the stack's own state was refused: $(cat "$work/own.said")"
[[ ! -e $work/own ]] || fault "the stack's own state is still there after its removal"

# -- the parent the stacks live in ----------------------------------------------

mkdir -p "$work/parent"
lay_out "$work/parent/stack-e" native
lay_out "$work/parent/stack-g" remote
printf 'kept\n' > "$work/parent/corpus.sha256"
state=$work/parent
status=0
remove_state > "$work/parent.said" 2>&1 || status=$?
((status != 0)) || fault "a directory no stack made was removed as a stack's state"
[[ -f $work/parent/stack-e/mode && -f $work/parent/stack-g/logs/up.log && -f $work/parent/corpus.sha256 ]] \
  || fault "what lives beside the stacks did not survive a refused removal"
grep -Fq 'not removing' "$work/parent.said" \
  || fault "the refusal does not say what it kept: $(cat "$work/parent.said")"

# -- a stack's state that holds another's ----------------------------------------

lay_out "$work/outer" remote
lay_out "$work/outer/inner" native
state=$work/outer
status=0
remove_state > "$work/outer.said" 2>&1 || status=$?
((status != 0)) || fault "a state holding another stack's state was removed"
[[ -f $work/outer/mode && -f $work/outer/inner/mode ]] \
  || fault "a refused removal took a stack's state with it"
grep -Fq "$work/outer/inner" "$work/outer.said" \
  || fault "the refusal does not name the other stack's state: $(cat "$work/outer.said")"

if ((faults)); then
  echo "test-state-removal: $faults fault(s)" >&2
  exit 1
fi
echo "test-state-removal: ok"
