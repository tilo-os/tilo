#!/usr/bin/env bash
# Filesystem policy probes must fail closed without pinning the async runtime.
# The network gate follows NetworkManager over D-Bus through tilo-nm, so the
# monitor forks no subprocess at all, and an unreadable state denies.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
monitor="$root/crates/system/fs/src/smart_monitor.rs"

grep -Fq 'tilo_nm::watch()' "$monitor"
grep -Fq 'allows_speculative_traffic()' "$monitor"
grep -Fq 'network.is_some_and(' "$monitor"
! grep -Eq 'nmcli|bounded_command::|std::process::Command|tokio::process::Command|\.output\(\)|wait_with_output\(' "$monitor"

echo 'filesystem subprocess boundary: ok'
