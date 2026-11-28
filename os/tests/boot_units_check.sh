#!/usr/bin/env bash
# Check a booted installed image on three things a working desktop can hide:
#
#   * no system unit failed. A failed unit marks the system degraded, and the
#     rollback confirmation refuses to confirm a degraded generation, so one
#     unrelated failure quietly leaves every update unconfirmed.
#   * tilo-rollback-confirm ran and succeeded, and on an enrolled machine it
#     confirmed the running generation. Its timer fires ten minutes after boot,
#     so the check starts it itself when the timer has not got there yet.
#   * no tilo service died on a syscall its own filter refuses (SIGSYS, the
#     kernel's audit type=1326), and the device's daemon has not restarted this
#     boot: a unit with Restart=on-failure is active again seconds later, so a
#     service that loses that call on every restart never shows as failed.
#   * the device's daemon learned the clock's synchronisation from timedated
#     over the system bus, since the kernel call its own filter refuses cannot
#     answer it: a boot that falls back to an unknown clock says nothing here.
#
# Run as root inside the guest, once the first session has come up:
#   sudo /usr/libexec/tilo/boot-units-check.sh
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

# A syscall a unit's filter refuses kills its process with SIGSYS: systemd
# records `status=31/SYS`, and the kernel's audit record is `type=1326`. A
# unit with `Restart=on-failure` comes back within seconds, so it is active
# again and never failed while it loses that call on every restart: the
# device's daemon crash-looped that way with this check green. The services a
# boot starts are what it must keep, so any such record from them is a failure.
if ! boot_log="$(journalctl -b -o cat --no-pager 2>/dev/null)"; then
  bad 'this boot has no readable journal: a refused syscall would go unseen'
  boot_log=
fi
refused="$(grep -E 'status=31/SYS|type=1326|SECCOMP' <<<"$boot_log" \
  | grep -E 'tilo[-a-z0-9@._]*\.service|comm="tilo|exe="/usr/(libexec/tilo/|bin/tilo)' || true)"
if [ -z "$refused" ]; then
  ok 'no tilo service was killed by a refused syscall'
else
  bad 'a tilo service was killed by a refused syscall (SIGSYS or seccomp):'
  printf '%s\n' "$refused" | tail -8 | sed 's/^/      /' >&2
fi
# The device's daemon is the machine's: a restart is a failure it recovered
# from, never a state a boot may end in.
runtime_restarts="$(systemctl show -p NRestarts --value tilo-runtime.service 2>/dev/null || true)"
case "$runtime_restarts" in
  0) ok 'the device daemon has not restarted this boot' ;;
  ''|*[!0-9]*) bad 'tilo-runtime.service restart count is unavailable' ;;
  *) bad "tilo-runtime.service restarted $runtime_restarts times this boot" ;;
esac

# The daemon asks systemd's timedated for the clock's state over the system bus, because the
# kernel call its own filter refuses cannot answer it; on Tilo OS chrony holds the clock, so it
# must report synchronised. A boot that says nothing fell back to an unknown clock.
clock_said="$(journalctl -b -q --no-pager -o cat -u tilo-runtime.service 2>/dev/null \
  | grep -F 'the clock is synchronised (timedated)' | tail -1)"
if [ -n "$clock_said" ]; then
  ok 'the device daemon learned the clock is synchronised (timedated)'
else
  bad 'tilo-runtime.service never reported the clock synchronised by timedated'
  journalctl -b --no-pager -o cat -u tilo-runtime.service 2>/dev/null \
    | grep -F 'the clock is ' | tail -3 | sed 's/^/      /' >&2 || true
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
