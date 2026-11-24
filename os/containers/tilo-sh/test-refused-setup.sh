#!/usr/bin/env bash
# A refused set-up step stops the stack at once, naming the step and its refusal: no step waits
# on the person a `tilo device add` asks, since no one here answers their card.
#
# No stack starts here: the stack's own person's commands are stand-ins, the founding refusing
# as a tilo that cannot make a person does. Held:
#
#   - a founding step that fails ends stack.sh non-zero, at once, naming the step and what it
#     said;
#   - a `device add` is refused before it can wait, naming the founding act that issues the
#     tickets the stack's computers join with.
#
# usage: test-refused-setup.sh
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/tilo-sh-refused.XXXXXX")
trap 'rm -rf "$work"' EXIT
faults=0
fault() {
  echo "FAIL: $*" >&2
  faults=$((faults + 1))
}

# The stack's own state, on a state of this test's own. Nothing of it is built, fetched or
# started.
export TILO_SH_STATE=$work/state TILO_SH_REMOTE=0
unset TILO_SH_LOUNGE
# shellcheck source=stack.sh
. "$here/stack.sh"

# The stack's person's own commands are stand-ins: no daemon starts, and the act that makes the
# person refuses.
owner_daemon() { return 0; }
as_owner() {
  case $* in
    "login --new-identity"*)
      printf 'tilo: no daemon made this computer a person\n' >&2
      return 1
      ;;
    *)
      printf 'a command the test does not stand in for: %s\n' "$*" >&2
      return 1
      ;;
  esac
}

# -- a refused founding step ---------------------------------------------------

started=$SECONDS
status=0
(stack_person) > "$work/founded" 2>&1 || status=$?
elapsed=$((SECONDS - started))
((status != 0)) || fault "a refused founding step ended 0"
((elapsed < 10)) || fault "a refused founding step took ${elapsed}s: nothing may wait for a person"
grep -Fq 'tilo made no person for the stack' "$work/founded" \
  || fault "the step that failed, with its refusal, is not named: $(cat "$work/founded")"

# -- the add, which no step of the stack may wait on ---------------------------

started=$SECONDS
status=0
(owner_person device add --server --json --no-wait) > "$work/add" 2>&1 || status=$?
elapsed=$((SECONDS - started))
((status != 0)) || fault "a device add in the stack ended 0"
((elapsed < 10)) || fault "a device add in the stack took ${elapsed}s: nothing may wait for a person"
grep -Fq 'founding act' "$work/add" \
  || fault "the add is not refused for the founding act that issues the tickets: $(cat "$work/add")"
status=0
(owner_tilo device add --server --json) > "$work/add-tilo" 2>&1 || status=$?
((status != 0)) || fault "a device add through owner_tilo ended 0"

if ((faults)); then
  echo "FAIL: $faults things stack.sh does with a refused set-up step did not hold" >&2
  exit 1
fi
echo "ok: a refused set-up step ends the stack at once, naming the step and its refusal, and the stack refuses a device add rather than wait for a person"
