#!/usr/bin/env bash
# os/dev/nearby-privileged-processes.sh, the probe the nearby-pairing gate runs
# on the joining machine, run as the gate runs it (`sh -s` behind a login
# shell's `-c`, the way ssh starts a remote command):
#   1. with nothing privileged running it passes. In particular the shells
#      around it, whose command lines name what it looks for, are not taken
#      for those processes; that mistake failed every release gate run.
#   2. each process it exists to catch (pkexec, a polkit agent's helper, the
#      home-peer control helper), whether known by its own name or by the
#      program its argv[0] names, makes it fail and is named.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
probe="$repo/os/dev/nearby-privileged-processes.sh"
names=(pkexec polkit-agent-helper-1 tilo-home-peer-control)

work="$(mktemp -d)"
fakes=()
cleanup() {
  local pid
  for pid in "${fakes[@]}"; do kill "$pid" 2>/dev/null || true; done
  rm -rf -- "$work"
}
trap cleanup EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# As ssh does: the account's shell runs the command line it was sent. That
# shell stays the probe's parent, and its command line names every process
# the probe looks for.
run_probe() {
  bash -c 'sh -s; status=$?; : pkexec polkit-agent-helper-1 tilo-home-peer-control; exit "$status"' \
    < "$probe" 2> "$work/probe.err"
}

# --- 1. a clean machine passes ---------------------------------------------------
run_probe || fail "the probe reported a pending process with none running: $(cat "$work/probe.err")"

# --- 2. each pending process is caught -------------------------------------------
await_start() {
  local pid=$1 comm=$2
  for _ in $(seq 1 50); do
    [ "$(cat "/proc/$pid/comm" 2>/dev/null)" = "$comm" ] && return 0
    sleep 0.1
  done
  fail "the stand-in for pid $pid never became $comm"
}
expect_caught() {
  local pid=$1 name=$2 how=$3 status=0
  run_probe || status=$?
  [ "$status" != 0 ] || fail "the probe missed a pending $name ($how)"
  [ "$status" = 1 ] || fail "the probe could not look for $name (status $status): $(cat "$work/probe.err")"
  grep -q "^pending: $pid .*$name" "$work/probe.err" \
    || fail "the probe did not name the pending $name ($how): $(cat "$work/probe.err")"
  kill "$pid"
  wait "$pid" 2>/dev/null || true
}
for name in "${names[@]}"; do
  # Known by its own name: the kernel names a process after the file it
  # executed, cut to 15 bytes.
  printf '#!/bin/sh\nwhile :; do sleep 0.2; done\n' > "$work/$name"
  chmod +x "$work/$name"
  "$work/$name" &
  pid=$!
  fakes+=("$pid")
  await_start "$pid" "${name:0:15}"
  expect_caught "$pid" "$name" "by its process name"

  # Known by the program its argv[0] names, as a helper started through a
  # wrapper is.
  bash -c "exec -a /usr/libexec/$name bash -c 'while :; do sleep 0.2; done'" &
  pid=$!
  fakes+=("$pid")
  for _ in $(seq 1 50); do
    tr '\0' '\n' < "/proc/$pid/cmdline" 2>/dev/null | head -n 1 | grep -qx "/usr/libexec/$name" && break
    sleep 0.1
  done
  expect_caught "$pid" "$name" "by its argv[0]"
done
run_probe || fail "the probe still reports a pending process after the stand-ins exited: $(cat "$work/probe.err")"
echo 'nearby privileged-process probe: ok'
