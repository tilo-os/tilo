#!/usr/bin/env bash
# Filesystem policy probes must fail closed without pinning the async runtime.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
monitor="$root/fs/src/smart_monitor.rs"

grep -Fq 'tokio::task::spawn_blocking' "$monitor"
grep -Fq 'bounded_command::run(' "$monitor"
grep -Fq 'NETWORK_PROBE_LIMIT' "$monitor"
grep -Fq 'NETWORK_PROBE_TIMEOUT' "$monitor"
! grep -Eq 'tokio::process::Command|\.output\(\)|wait_with_output\(' "$monitor"

echo 'filesystem subprocess boundary: ok'
