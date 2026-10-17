#!/usr/bin/env bash
# Credential access belongs only to the trusted, isolated execution check.
set -euo pipefail
umask 077
: "${RYSTIC_CI_TOKEN:?configure the RYSTIC_CI_TOKEN t secret for trusted runs}"
artifacts=${CARGO_TARGET_DIR:?t must supply the build artifact directory}/rystic-ci
report_dir=$CARGO_TARGET_DIR/rystic-results
mkdir -p "$report_dir"
home=$(mktemp -d)
cleanup() { rm -rf -- "$home"; }
trap cleanup EXIT
child=
interrupt() {
  local sig=$1 code=$2
  trap '' INT TERM
  if [[ -n $child ]]; then
    kill -s "$sig" "$child" 2>/dev/null || true
    # Keep the token available until Python's bounded stop/remove has finished.
    wait "$child" || true
  fi
  exit "$code"
}
trap 'interrupt INT 130' INT
trap 'interrupt TERM 143' TERM
printf '%s' "$RYSTIC_CI_TOKEN" > "$home/token"
unset RYSTIC_CI_TOKEN
export RYSTIC_HOME=$home
args=()
case ${1:-supported} in
  supported) ;;
  strict) args+=(--strict) ;;
  *) printf 'unknown contract mode\n' >&2; exit 2 ;;
esac
# The Python runner captures provider output and emits only sanitized JSON.
python3 tools/ci/rystic_ci.py run --artifacts "$artifacts" \
  "${args[@]}" > "$report_dir/${1:-supported}.json" &
child=$!
status=0
wait "$child" || status=$?
cat "$report_dir/${1:-supported}.json"
exit "$status"
