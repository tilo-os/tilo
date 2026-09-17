#!/usr/bin/env bash
# Shell-owned helper boundaries must cap allocation, duration, and descendants.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
projection="$root/shell/src/shell/projection.rs"
capture="$root/shell/src/compositor/audio_capture.rs"

grep -Fq 'bounded_command::run_prepared(' "$projection"
grep -Fq 'PROJECTOR_OUTPUT_LIMIT' "$projection"
grep -Fq 'PROJECTOR_TIMEOUT' "$projection"
! grep -Eq '\.output\(\)|wait_with_output\(' "$projection"

grep -Fq 'bounded_command::run_prepared(' "$capture"
grep -Fq '.process_group(0)' "$capture"
grep -Fq 'libc::kill(group, libc::SIGKILL)' "$capture"

echo 'shell subprocess boundary: ok'
