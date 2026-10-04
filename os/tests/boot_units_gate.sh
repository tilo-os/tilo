#!/usr/bin/env bash
# Gate a booted installed image on two things a working desktop can hide:
#
#   * no system unit failed. A failed unit marks the system degraded, and the
#     rollback confirmation refuses to confirm a degraded generation, so one
#     unrelated failure quietly leaves every update unconfirmed.
#   * tilo-rollback-confirm ran and succeeded, and on an enrolled machine it
#     confirmed the running generation. Its timer fires ten minutes after boot,
#     so the gate starts it itself when the timer has not got there yet.
#
# Run as root inside the guest, once the first session has come up:
#   sudo /usr/libexec/tilo/boot-units-gate.sh
#
# A unit that may legitimately be failed is listed in ALLOWED_FAILED with the
# reason; the list is empty because none may.
set -uo pipefail

declare -A ALLOWED_FAILED=()

cmdline="${TILO_PROC_CMDLINE:-/proc/cmdline}"
state_file="${TILO_ROLLBACK_STATE:-/var/lib/tilo/rollback/state.json}"
settle_seconds="${TILO_BOOT_SETTLE_SEC:-300}"
confirm=tilo-rollback-confirm.service
fail=0

ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# Startup jobs still queued are not failures yet; wait for the manager to
# finish (is-system-running --wait returns once it is no longer "starting").
state="$(timeout "$settle_seconds" systemctl is-system-running --wait 2>/dev/null || true)"
printf 'system state: %s\n' "${state:-unknown}"

confirm_log() { journalctl -b --no-pager -o cat -u "$confirm" 2>/dev/null || true; }

if ! systemctl cat "$confirm" >/dev/null 2>&1; then
  bad "$confirm is not installed"
else
  started="$(systemctl show -p ExecMainStartTimestampMonotonic --value "$confirm" 2>/dev/null || true)"
  if [ -z "$started" ] || [ "$started" = 0 ]; then
    printf '%s has not run yet on this boot; starting it now\n' "$confirm"
    systemctl start "$confirm" || true
  fi
  result="$(systemctl show -p Result --value "$confirm" 2>/dev/null || true)"
  status="$(systemctl show -p ExecMainStatus --value "$confirm" 2>/dev/null || true)"
  if [ "$result" = success ] && [ "${status:-0}" = 0 ]; then
    ok "$confirm succeeded"
  else
    bad "$confirm result: ${result:-unknown}, exit status ${status:-unknown}"
    confirm_log | tail -20 | sed 's/^/      /' >&2
  fi

  generation="$(tr -s ' \t' '\n' <"$cmdline" 2>/dev/null \
    | sed -n 's/^tilo\.generation=\([0-9][0-9]*\)$/\1/p' | head -1)"
  log="$(confirm_log)"
  if grep -Fq "confirming generation" <<<"$log"; then
    confirmed="$(tr -d ' \n\t' <"$state_file" 2>/dev/null \
      | sed -n 's/.*"last_confirmed":\([0-9][0-9]*\).*/\1/p')"
    if [ -n "$generation" ] && [ "$confirmed" = "$generation" ]; then
      ok "generation $generation is confirmed"
    else
      bad "$confirm went ahead but the confirmed generation is ${confirmed:-unreadable}, not ${generation:-<none on the command line>}"
    fi
  elif [ -e "$state_file" ]; then
    bad "this machine is enrolled, but $confirm did not confirm generation ${generation:-<none>}:"
    grep -F 'tilo-rollback:' <<<"$log" | tail -5 | sed 's/^/      /' >&2
  else
    reason="$(grep -F 'tilo-rollback:' <<<"$log" | tail -1)"
    ok "nothing to confirm on this machine (${reason:-no reason logged})"
  fi
fi

failed="$(systemctl list-units --state=failed --no-legend --plain 2>/dev/null | awk '{print $1}')"
unexpected=()
while IFS= read -r unit; do
  [ -n "$unit" ] || continue
  if [ -n "${ALLOWED_FAILED[$unit]:-}" ]; then
    printf 'allowed failed unit %s: %s\n' "$unit" "${ALLOWED_FAILED[$unit]}"
  else
    unexpected+=("$unit")
  fi
done <<<"$failed"
if [ "${#unexpected[@]}" -eq 0 ]; then
  ok 'no unit failed on this boot'
else
  bad "${#unexpected[@]} unit(s) failed on this boot:"
  for unit in "${unexpected[@]}"; do
    printf '  %s\n' "$unit" >&2
    journalctl -b --no-pager -o cat -u "$unit" 2>/dev/null | tail -8 | sed 's/^/      /' >&2
  done
fi

exit "$fail"
