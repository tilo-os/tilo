#!/usr/bin/env bash
# os/tests/boot_units_gate.sh against a stubbed systemd: what it accepts, and
# that each way a boot can go wrong fails it.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
gate="$repo/os/tests/boot_units_gate.sh"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

cat >"$work/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  'is-system-running --wait') echo "${STUB_STATE:-running}" ;;
  'cat tilo-rollback-confirm.service') [ "${STUB_CONFIRM_INSTALLED:-yes}" = yes ] ;;
  'show -p ExecMainStartTimestampMonotonic --value tilo-rollback-confirm.service')
    if [ -e "$STUB_DIR/started" ]; then echo 123; else echo "${STUB_CONFIRM_STARTED:-0}"; fi ;;
  'start tilo-rollback-confirm.service') touch "$STUB_DIR/started" ;;
  'show -p Result --value tilo-rollback-confirm.service') echo "${STUB_CONFIRM_RESULT:-success}" ;;
  'show -p ExecMainStatus --value tilo-rollback-confirm.service') echo "${STUB_CONFIRM_STATUS:-0}" ;;
  'list-units --state=failed --no-legend --plain')
    for unit in ${STUB_FAILED:-}; do printf '%s loaded failed failed Something\n' "$unit"; done ;;
  *) echo "unexpected systemctl $*" >&2; exit 99 ;;
esac
EOF
cat >"$work/bin/journalctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${STUB_CONFIRM_LOG:-}"
EOF
cat >"$work/bin/timeout" <<'EOF'
#!/usr/bin/env bash
shift; exec "$@"
EOF
chmod +x "$work/bin/"*

printf 'root=tilo-auto tilo.generation=7 quiet\n' >"$work/cmdline"

run_gate() {
  rm -f "$work/started"
  env PATH="$work/bin:$PATH" STUB_DIR="$work" TILO_PROC_CMDLINE="$work/cmdline" \
    TILO_ROLLBACK_STATE="$work/state.json" "$@" bash "$gate" >"$work/out" 2>&1
}
expect_pass() {
  local name="$1"; shift
  run_gate "$@" || { cat "$work/out"; echo "FAIL: $name should pass" >&2; exit 1; }
}
expect_fail() {
  local name="$1" needle="$2"; shift 2
  if run_gate "$@"; then cat "$work/out"; echo "FAIL: $name should fail" >&2; exit 1; fi
  grep -Fq "$needle" "$work/out" || { cat "$work/out"; echo "FAIL: $name should report: $needle" >&2; exit 1; }
}

rm -f "$work/state.json"
expect_pass 'an unenrolled machine with nothing failed' \
  STUB_CONFIRM_LOG='tilo-rollback: no TPM on this machine — rollback binding is unavailable by design'
grep -Fq 'starting it now' "$work/out" || { cat "$work/out"; echo 'FAIL: the gate must start a confirmation the timer has not run' >&2; exit 1; }
grep -Fq 'nothing to confirm on this machine (tilo-rollback: no TPM' "$work/out" \
  || { cat "$work/out"; echo 'FAIL: the gate must say why there was nothing to confirm' >&2; exit 1; }

expect_pass 'a confirmation the timer already ran' STUB_CONFIRM_STARTED=42
if grep -Fq 'starting it now' "$work/out"; then
  cat "$work/out"; echo 'FAIL: the gate must not run a finished confirmation again' >&2; exit 1
fi

# The field failure: the sandbox could not be set up, so the unit failed with
# 226/NAMESPACE and is itself the failed unit.
expect_fail 'a confirmation that could not start' 'result: exit-code, exit status 226' \
  STUB_CONFIRM_RESULT=exit-code STUB_CONFIRM_STATUS=226 STUB_FAILED=tilo-rollback-confirm.service
grep -Fq '1 unit(s) failed on this boot' "$work/out" \
  || { cat "$work/out"; echo 'FAIL: the failed confirmation must be listed as a failed unit' >&2; exit 1; }

expect_fail 'an unrelated failed unit' 'tilo-relay.service' STUB_FAILED='tilo-relay.service'

expect_fail 'a missing confirmation unit' 'tilo-rollback-confirm.service is not installed' \
  STUB_CONFIRM_INSTALLED=no

printf '{"nv_index":1,"nv_base":0,"last_confirmed":7,"foreign_keyslots":[0]}\n' >"$work/state.json"
expect_pass 'an enrolled machine that confirmed its generation' \
  STUB_CONFIRM_LOG='tilo-rollback: confirming generation 7 on /dev/vda3'

printf '{"nv_index":1,"nv_base":0,"last_confirmed":6,"foreign_keyslots":[0]}\n' >"$work/state.json"
expect_fail 'an enrolled machine left on the previous generation' 'confirmed generation is 6, not 7' \
  STUB_CONFIRM_LOG='tilo-rollback: confirming generation 7 on /dev/vda3'

expect_fail 'an enrolled machine whose confirmation declined' 'did not confirm generation 7' \
  STUB_CONFIRM_LOG="tilo-rollback: WARNING: system state is 'degraded', not 'running' — not confirming this generation"

echo 'ok    boot units gate'
