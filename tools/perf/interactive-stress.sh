#!/usr/bin/env bash
# Does the desktop stay interactive while batch work eats the machine?
#
#   tools/perf/interactive-stress.sh [--duration SECONDS] [--vm-workers N]
#                                    [--vm-bytes SIZE] [--build CMD] [--slice UNIT]
#
# Runs `stress-ng --vm` (anonymous memory churn, enough to fill RAM and swap)
# and, optionally, a parallel build in the slice batch work lands in by
# default (rootless containers and the background tier both resolve to the
# user manager's batch slices; the default here is background.slice), then
# measures what the person would feel, against budgets:
#
#   * pressure stall time of the compositor's session scope (cpu, memory, io):
#     the kernel's own count of time its threads waited. This is the direct
#     "compositor stalled" signal and needs no cooperation from the compositor.
#   * libinput "system is too slow" reports the compositor logged meanwhile.
#   * app launches: each app in LAUNCH_APPS is opened through the action model
#     and must show up in `tilo-control windows` within MAX_LAUNCH_SECONDS.
#
# Exit 0 inside budget, 1 outside, 2 when the check cannot run. Run it as the
# desktop's own user, from a session started by tilo-session. It changes no
# system setting: the load runs in a transient unit that is stopped at the end.
#
# Budgets (environment): MAX_MEM_FULL_PCT=1.0 MAX_IO_FULL_PCT=2.0
# MAX_CPU_SOME_PCT=10 MAX_SLOW_REPORTS=0 MIN_LAUNCH_SUCCESS=100
# MAX_LAUNCH_SECONDS=5 LAUNCH_APPS="so.tilo.calculator so.tilo.file-explorer"
set -euo pipefail

duration=60
vm_workers=$(( $(nproc) / 2 ))
vm_bytes=90%
build=
slice=background.slice
while [ $# -gt 0 ]; do
  case $1 in
    --duration) duration=$2; shift 2 ;;
    --vm-workers) vm_workers=$2; shift 2 ;;
    --vm-bytes) vm_bytes=$2; shift 2 ;;
    --build) build=$2; shift 2 ;;
    --slice) slice=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
max_mem_full=${MAX_MEM_FULL_PCT:-1.0}
max_io_full=${MAX_IO_FULL_PCT:-2.0}
max_cpu_some=${MAX_CPU_SOME_PCT:-10}
max_slow=${MAX_SLOW_REPORTS:-0}
min_launch=${MIN_LAUNCH_SUCCESS:-100}
max_launch_s=${MAX_LAUNCH_SECONDS:-5}
launch_apps=${LAUNCH_APPS:-so.tilo.calculator so.tilo.file-explorer}
log=${TILO_SESSION_LOG:-$HOME/.local/state/tilo/session-logs/latest.log}
control=${TILO_CONTROL:-tilo-control}

for tool in stress-ng systemd-run awk "$control"; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 2; }
done
shell_pid=$(pgrep -xo shell || true)
[ -n "$shell_pid" ] || { echo "no running compositor" >&2; exit 2; }
session_cg=/sys/fs/cgroup$(sed -n 's/^0:://p' "/proc/$shell_pid/cgroup")
[ -r "$session_cg/memory.pressure" ] || { echo "no pressure files at $session_cg" >&2; exit 2; }

# total stall microseconds of one pressure line: pressure KIND some|full
stall_us() { awk -v k="$2" '$1==k { sub("total=","",$5); print $5 }' "$session_cg/$1.pressure"; }
slow_reports() { grep -a -c 'system is too slow' "$log" 2>/dev/null || true; }

snap() { printf '%s %s %s %s\n' "$(stall_us memory full)" "$(stall_us io full)" \
  "$(stall_us cpu some)" "$(slow_reports)"; }

load=tilo-stress-$$
cleanup() { systemctl --user stop "$load.service" "$load-build.service" 2>/dev/null || true; }
trap cleanup EXIT

read -r mem0 io0 cpu0 slow0 < <(snap)
t0=$(date +%s%N)

systemd-run --user --quiet --collect --slice="$slice" --unit="$load" \
  stress-ng --vm "$vm_workers" --vm-bytes "$vm_bytes" --vm-keep --timeout "${duration}s"
if [ -n "$build" ]; then
  systemd-run --user --quiet --collect --slice="$slice" --unit="$load-build" \
    bash -c "$build"
fi
sleep 5 # let the load reach its footprint before probing

ok=0 total=0 slowest=0
for app in $launch_apps; do
  total=$((total + 1))
  before=$("$control" windows 2>/dev/null | wc -l)
  start=$(date +%s%N)
  "$control" actions invoke applications "$app" >/dev/null 2>&1 || true
  seen=0
  while [ $(( ($(date +%s%N) - start) / 1000000 )) -lt $((max_launch_s * 1000)) ]; do
    if [ "$("$control" windows 2>/dev/null | wc -l)" -gt "$before" ]; then seen=1; break; fi
    sleep 0.2
  done
  ms=$(( ($(date +%s%N) - start) / 1000000 ))
  [ "$seen" = 1 ] && ok=$((ok + 1))
  [ "$ms" -gt "$slowest" ] && slowest=$ms
  printf 'launch %-28s %s in %d ms\n' "$app" "$([ "$seen" = 1 ] && echo ok || echo FAILED)" "$ms"
done

# keep loading for the remainder of the window
elapsed=$(( ($(date +%s%N) - t0) / 1000000000 ))
[ "$elapsed" -lt "$duration" ] && sleep $((duration - elapsed))

read -r mem1 io1 cpu1 slow1 < <(snap)
wall_us=$(( ($(date +%s%N) - t0) / 1000 ))
pct() { awk -v d="$1" -v w="$wall_us" 'BEGIN { printf "%.2f", 100 * d / w }'; }
mem_pct=$(pct $((mem1 - mem0))); io_pct=$(pct $((io1 - io0))); cpu_pct=$(pct $((cpu1 - cpu0)))
slow=$((slow1 - slow0))
success=$(( total == 0 ? 100 : 100 * ok / total ))

printf 'session scope %s\n' "${session_cg#/sys/fs/cgroup}"
printf 'memory full %s%% (budget %s)  io full %s%% (budget %s)  cpu some %s%% (budget %s)\n' \
  "$mem_pct" "$max_mem_full" "$io_pct" "$max_io_full" "$cpu_pct" "$max_cpu_some"
printf 'libinput slow reports %d (budget %d)  launches %d/%d ok, slowest %d ms (budget %d)\n' \
  "$slow" "$max_slow" "$ok" "$total" "$slowest" $((max_launch_s * 1000))

fail=0
over() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a > b) }'; }
over "$mem_pct" "$max_mem_full" && { echo "FAIL memory stall over budget" >&2; fail=1; }
over "$io_pct" "$max_io_full" && { echo "FAIL io stall over budget" >&2; fail=1; }
over "$cpu_pct" "$max_cpu_some" && { echo "FAIL cpu stall over budget" >&2; fail=1; }
[ "$slow" -le "$max_slow" ] || { echo "FAIL compositor reported a too-slow system" >&2; fail=1; }
[ "$success" -ge "$min_launch" ] || { echo "FAIL app launches under load" >&2; fail=1; }
[ "$fail" = 0 ] && echo 'PASS interactive under load'
exit "$fail"
