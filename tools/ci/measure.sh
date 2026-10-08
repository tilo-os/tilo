#!/usr/bin/env bash
# Run a CI command and publish wall-clock and process CPU usage. Collect numbers
# per selected shard so package weights can be tuned without another compilation
# pass.
set -euo pipefail

label="${1:?usage: tools/ci/measure.sh <label> -- <command...>}"
shift
[ "${1:-}" = "--" ] && shift
[ "$#" -gt 0 ] || { echo "tools/ci/measure.sh: no command given" >&2; exit 2; }

timing="$(mktemp -t ci-measure-XXXXXX)"
trap 'rm -f "$timing"' EXIT

cgroup_root="${TILO_CGROUP_ROOT:-/sys/fs/cgroup}"
memory_events_before=""
if [ -r "$cgroup_root/memory.events" ]; then
  memory_events_before="$(cat "$cgroup_root/memory.events")"
fi

cpu_stat_before="unavailable"
if [ -r "$cgroup_root/cpu.stat" ]; then
  cpu_stat_before=$(tr '\n' ' ' < "$cgroup_root/cpu.stat")
fi

status=0
if command -v /usr/bin/time >/dev/null 2>&1; then
  /usr/bin/time -f 'wall_seconds=%e\nuser_seconds=%U\nsystem_seconds=%S\nmax_rss_kib=%M' -o "$timing" "$@" || status=$?
else
  start="$(date +%s)"
  "$@" || status=$?
  printf 'wall_seconds=%s\nuser_seconds=unavailable\nsystem_seconds=unavailable\nmax_rss_kib=unavailable\n' \
    "$(( $(date +%s) - start ))" > "$timing"
fi

# Record the actual inner job envelope. Pod/QEMU metrics cannot establish
# Docker's quota or allowed CPU set, and interactive CRI exec may be unavailable.
for cpu_file in cpu.max cpuset.cpus.effective; do
  cpu_value="unavailable"
  if [ -r "$cgroup_root/$cpu_file" ]; then
    cpu_value=$(cat "$cgroup_root/$cpu_file")
  fi
  printf 'cgroup_%s=%s\n' "${cpu_file//./_}" "$cpu_value" >> "$timing"
done
printf 'cgroup_cpu_stat_before=%s\n' "$cpu_stat_before" >> "$timing"
cpu_stat_after="unavailable"
if [ -r "$cgroup_root/cpu.stat" ]; then
  cpu_stat_after=$(tr '\n' ' ' < "$cgroup_root/cpu.stat")
fi
printf 'cgroup_cpu_stat_after=%s\n' "$cpu_stat_after" >> "$timing"

# The container's peak memory so far, success or not: the headroom job
# parallelism is tuned against.
if [ -r "$cgroup_root/memory.peak" ]; then
  echo "cgroup_memory_peak=$(cat "$cgroup_root/memory.peak")" >> "$timing"
fi
echo "==> ${label} measurements"
sed 's/^/    /' "$timing"
if [ "$status" -ne 0 ] && [ -r "$cgroup_root/memory.events" ]; then
  [ ! -r "$cgroup_root/memory.max" ] || echo "    cgroup_memory_max=$(cat "$cgroup_root/memory.max")"
  echo "    cgroup_memory_events_before=$(printf '%s' "$memory_events_before" | tr '\n' ' ')"
  echo "    cgroup_memory_events_after=$(tr '\n' ' ' < "$cgroup_root/memory.events")"
fi
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### ${label} resource use"
    echo '```text'
    cat "$timing"
    if [ "$status" -ne 0 ] && [ -r "$cgroup_root/memory.events" ]; then
      [ ! -r "$cgroup_root/memory.max" ] || echo "cgroup_memory_max=$(cat "$cgroup_root/memory.max")"
      echo "cgroup_memory_events_before=$(printf '%s' "$memory_events_before" | tr '\n' ' ')"
      echo "cgroup_memory_events_after=$(tr '\n' ' ' < "$cgroup_root/memory.events")"
    fi
    echo '```'
  } >> "$GITHUB_STEP_SUMMARY"
fi

exit "$status"
