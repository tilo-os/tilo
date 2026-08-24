#!/usr/bin/env bash
# Reap qOS processes nobody owns any more, before starting anything that
# needs the GPU.
#
# Why this exists, from the kernel's own account of a freeze:
#
#   amdgpu: VM memory stats for proc shell(2523673) ... is non-zero when fini
#   amdgpu: VM memory stats for proc file-explorer(2528974) ... non-zero when fini
#   amdgpu: VM memory stats for proc file-explorer(2525274) ... non-zero when fini
#   amdgpu: VM memory stats for proc word-processor(2532222) ... non-zero when fini
#   amdgpu: VM memory stats for proc chat(2530364) ... non-zero when fini
#
# Three file explorers, two word processors, a chat and a compositor, all
# dying with GPU allocations outstanding. A later boot got the same story
# from the other driver: `NVRM: Out of memory [NV_ERR_NO_MEMORY]`.
#
# One demo run starts three compositors and up to four applications each.
# Every one of them holds a GPU context on the same device driving the
# desktop you are sitting in front of. Runs that are interrupted -- Ctrl-C,
# a killed harness, a crash between spawn and teardown -- leave those
# contexts behind, and the next run adds its own on top. Nothing reclaims
# them until reboot, which is why the machine had to be rebooted five times
# in one day.
#
# The rigs do try: they set `die_with_parent` and track what they spawn.
# That covers the parent exiting cleanly. It does not cover the parent
# being SIGKILLed, which is exactly the case that leaks.
#
# So this is the pre-flight: nothing that renders should start while
# somebody else's orphans still hold the device.
#
#   scripts/demo-guard.sh          # report and reap
#   scripts/demo-guard.sh --check  # report only, non-zero if anything found

set -uo pipefail

CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

# Scopes first, and they are the real answer.
#
# A run lives in a transient systemd scope, so stopping the scope takes
# the whole cgroup — including everything this script has never heard of.
# That matters more than it sounds: reaping by name left six
# `xdg-dbus-proxy` processes alive for half an hour, one per sandboxed
# application, in two scopes systemd still reported as *running*. Because
# live tasks remained, the scope never terminated and `--collect` never
# disposed of it.
#
# The name list below cannot fix that by growing. A run spawns dbus
# proxies, the driving rig, ffmpeg, and whatever a launcher decides an
# application needs; enumerating them is the job the cgroup already does.
# So the cgroup is asked first, and the names are only the fallback for
# strays that predate the wrapper or were started outside it.
scopes() { systemctl --user list-units --all --no-pager 2>/dev/null \
    | awk '/qos-demo-[0-9]+\.scope/ {print $1}'; }

mapfile -t SCOPES < <(scopes)
if [ "${#SCOPES[@]}" -gt 0 ]; then
  echo "demo-guard: ${#SCOPES[@]} demo scope(s) still registered:"
  printf '  %s\n' "${SCOPES[@]}"
  if [ "$CHECK_ONLY" -eq 1 ]; then
    echo "demo-guard: --check given, nothing stopped"
    exit 1
  fi
  systemctl --user stop "${SCOPES[@]}" 2>/dev/null
  sleep 1
  mapfile -t SCOPES < <(scopes)
  if [ "${#SCOPES[@]}" -gt 0 ]; then
    echo "demo-guard: ${#SCOPES[@]} scope(s) would not stop — investigate"
    exit 1
  fi
  echo "demo-guard: scopes stopped, cgroups gone"
fi

# Matched on `comm`, not on the full command line. A pattern matched
# against `args` also matches *this script* and the pipeline reading it,
# which is how a wait loop here once spun forever against itself.
PATTERN='^(shell|chat|photos|file-explorer|word-processor|qos-agent|qos-launcher|shots)$'

mapfile -t FOUND < <(ps -eo pid,ppid,etime,comm --no-headers | awk -v p="$PATTERN" '$4 ~ p {print $1" "$2" "$3" "$4}')

if [ "${#FOUND[@]}" -eq 0 ]; then
  echo "demo-guard: no qOS render processes running — clear to start"
  exit 0
fi

echo "demo-guard: ${#FOUND[@]} qOS render process(es) already hold the GPU:"
printf '  %s\n' "${FOUND[@]}"

if [ "$CHECK_ONLY" -eq 1 ]; then
  echo "demo-guard: --check given, nothing reaped"
  exit 1
fi

# TERM first so a compositor can release its device the way it knows how;
# a context torn down by the driver after SIGKILL is the state the log
# above is complaining about.
for row in "${FOUND[@]}"; do
  pid="${row%% *}"
  kill -TERM "$pid" 2>/dev/null
done

for _ in 1 2 3 4 5 6 7 8 9 10; do
  sleep 0.5
  remaining=$(ps -eo comm --no-headers | grep -cE "$PATTERN")
  [ "$remaining" -eq 0 ] && break
done

remaining=$(ps -eo comm --no-headers | grep -cE "$PATTERN")
if [ "$remaining" -gt 0 ]; then
  echo "demo-guard: $remaining did not exit on TERM; sending KILL"
  ps -eo pid,comm --no-headers | awk -v p="$PATTERN" '$2 ~ p {print $1}' \
    | while read -r pid; do kill -KILL "$pid" 2>/dev/null; done
  sleep 1
fi

left=$(ps -eo comm --no-headers | grep -cE "$PATTERN")
if [ "$left" -gt 0 ]; then
  echo "demo-guard: $left still alive — do NOT start a run; investigate first"
  exit 1
fi
echo "demo-guard: reaped, GPU clear"
