#!/usr/bin/env bash
# Run one of the session's end-to-end proofs (scripts/headless-examples/README.md, "Proofs"), on
# the session build TILO_NESTED_SESSION_BUILD names (dist/dev/nested-session-build.sh's), which
# it never builds; `tilolang` takes the collaboration probe from TILO_COLLAB_PROBE. Each proof starts headless sessions of its own on scratch homes, so nothing
# here touches a desktop. Its verdict (summary.json), what it said (proof.log), its screenshots
# and the sessions' logs land in $CARGO_TARGET_DIR/e2e/PROOF, the `e2e/PROOF` checks' outputs.
#
#   tools/ci/session-proofs.sh tilolang|two-people|mail-two-members|agent-sessions|session-lock
set -euo pipefail

proof=${1:?usage: $0 tilolang|two-people|mail-two-members|agent-sessions|session-lock}
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
cd "$repo_root"

target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
build=$(realpath -m "${TILO_NESTED_SESSION_BUILD:-$target/nested-session}")
[[ -x $build/bin/tilo-session ]] || { echo "no session build in $build (dist/dev/nested-session-build.sh)" >&2; exit 1; }
export TILO_NESTED_SESSION_BUILD=$build
export TILO_SHELL_BINARY=$build/bin/tilo-session
export TILO_CONTROL_BINARY=$build/bin/tilo-control
# The proofs are scripts, not modules: nothing may drop bytecode into the tree.
export PYTHONDONTWRITEBYTECODE=1

out=$target/e2e/$proof
run=$(mktemp -d)
rm -rf -- "$out"
mkdir -p -- "$out"
# What a person reads a run by: the verdict, the screenshots and the logs, not
# the sessions' installed state.
keep() {
    (cd "$run" && find . -maxdepth 3 -type f \( -name '*.json' -o -name '*.png' -o -name '*.log' \) \
        -size -20M -exec cp --parents -t "$out" {} +)
    rm -rf -- "$run"
}
trap keep EXIT

scripts=scripts/headless-examples
status=0
case $proof in
    tilolang) python3 "$scripts/tilolang-e2e.py" --no-build --out "$run" ;;
    two-people) python3 "$scripts/hosted-names-proof.py" --out "$run" ;;
    mail-two-members) python3 "$scripts/mail-two-members-proof.py" --out "$run" ;;
    agent-sessions|session-lock) python3 "$scripts/$proof-proof.py" ;;
    *) echo "no proof named $proof" >&2; exit 2 ;;
esac 2>&1 | tee "$out/proof.log" || status=$?
# A failed run keeps no outputs: the end of each session's log is in the check's own.
if ((status)); then
    while IFS= read -r -d '' log; do
        printf '=== the end of %s\n' "${log#"$run"/}"
        # Less the index's warning about records of apps this session does not carry, which
        # repeats several times a second and would leave nothing else in view.
        grep -v "a record's app reads none of it" -- "$log" | tail -n 80 || true
    done < <(find "$run" -maxdepth 3 -name 'session*.log' -print0)
fi
# A proof that writes no verdict of its own gets one: what it said is proof.log.
[[ -f $run/summary.json ]] || printf '{"proof": "%s", "passed": %s}\n' "$proof" \
    "$( ((status)) && echo false || echo true)" >"$out/summary.json"
exit "$status"
