#!/usr/bin/env bash
# The public room host's service level at its declared capacity, on a host
# nobody else shares: the room-load bench measures the workload with the
# machine to itself, with 70 % of the host's busiest second held back, and past
# the room's own guest cap. tools/perf/room_capacity.py judges it against the
# budget; this script sets the
# degradation and nothing else.
#
#   perf-room-degraded.sh [command [args...]]    a command, run under it
#
# Why steal CPU instead of loading it: the bench starts the host, its guests
# and its driver on one node, so the host's capacity cannot be scaled down with
# `--host-threads` alone. A cgroup that may use 30 % of one CPU idles the host
# by exactly that share of its own work: at the host's busiest second it is the
# host's headroom that disappears, and the guest driver, which is never held
# back, keeps measuring (a slower driver would move the very latencies the
# probe reads).
#
# Everything happens inside the namespaces this script creates for itself, as
# an unprivileged user (user, mount, PID and network namespaces): the link it
# shapes belongs to that namespace, so nothing outside it is throttled or
# shaped — not this machine, not another job's work — and no root is needed.
# Inside them every process is this command's, which is why the hold is applied
# to all of them and to the ones the bench starts later.
set -euo pipefail

# Keeps the rest of each second off the host ($STOLEN_PERCENT through the
# scheduler class below): whatever the host does with its share, it does it
# 70 % slower than alone.
STOLEN_PERCENT=70

die() { printf '%s\n' "$*" >&2; exit 2; }

# The batch scheduler class holds back every process it names and costs the
# machine nothing else. Inside the PID namespace every process is this run's,
# which is what makes it settable without a capability; `chrt` may still refuse
# it on a kernel that wants SCHED_BATCH for CAP_SYS_NICE on other uids, and
# then a process goes unheld rather than the run failing half-degraded.
throttle() {
  command -v chrt >/dev/null || die "chrt is missing: the degradation cannot be applied"
  local pid
  for pid in /proc/[0-9]*; do
    chrt -b 0 -p "${pid#/proc/}" >/dev/null 2>&1 || true
  done
}

# The link the guests dial: this namespace's loopback, shaped on this
# namespace's queue discipline. 3 Mbit/s down and up, which is under what the
# workload's cursor traffic needs at the declared capacity.
shape() {
  command -v tc >/dev/null || die "tc is missing: the link to the host cannot be made slower"
  tc qdisc replace dev lo root handle 1: tbf rate 3000kbit burst 128kbit latency 400ms
  trap 'tc qdisc del dev lo root 2>/dev/null || true' EXIT
}

inner() {
  shape
  # Before the command starts, so the first thing it does is already held back,
  # and a tool that is missing fails the run here rather than inside the sweep.
  throttle
  status=0
  # Re-applied for as long as the command runs: the bench starts the host
  # after this, and chrt takes one process at a time. It is stopped before the
  # script returns, so nothing of it outlives the run.
  ( while sleep 0.5; do throttle; done ) &
  sweeper=$!
  "$@" || status=$?
  kill "$sweeper" 2>/dev/null || true
  wait "$sweeper" 2>/dev/null || true
  return "$status"
}

# New user, mount, PID and network namespaces: the queue discipline and the
# scheduling classes it sets belong to them, and everything the command starts
# stays in them. Not mapped to uid 0 inside (that takes CAP_SETFCAP, which a
# job does not hold): an unprivileged user may set its own scheduling class and
# shape a link of a network namespace it created. The script then runs itself
# again inside, as --inner.
if [[ ${1:-} == --inner ]]; then
  shift
  inner "$@"
  exit $?
fi
[[ $# -gt 0 ]] || die "usage: perf-room-degraded.sh command [args...]"
command -v unshare >/dev/null || die "unshare is missing: nothing can be degraded without it"
unshare --user --mount --pid --net --fork --kill-child -- "${0}" --inner "$@"
