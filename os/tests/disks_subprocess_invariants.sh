#!/usr/bin/env bash
# Removable and malformed devices must not turn disk helpers into DoS inputs.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
probe="$root/apps/disks/src/probe.rs"
ops="$root/apps/disks/src/ops.rs"

grep -Fq 'bounded_command::run(' "$probe"
grep -Fq 'PROBE_OUTPUT_LIMIT' "$probe"
grep -Fq 'PROBE_TIMEOUT' "$probe"
! grep -Eq '\.output\(\)|wait_with_output\(' "$probe"

grep -Fq 'bounded_command::run_prepared(' "$ops"
! grep -Eq '\.output\(\)|wait_with_output\(' "$ops"

echo 'disk subprocess boundary: ok'
