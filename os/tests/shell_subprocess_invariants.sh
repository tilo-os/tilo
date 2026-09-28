#!/usr/bin/env bash
# Shell-owned helper boundaries must cap allocation, duration, and descendants.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
projection="$root/crates/session/shell/src/shell/projection.rs"
capture="$root/crates/session/shell/src/compositor/audio_capture.rs"
catalogue="$root/crates/session/shell/src/shell/catalogue.rs"
history="$root/crates/session/shell/src/shell/history_narrative.rs"
firstboot="$root/crates/session/shell/src/firstboot.rs"

grep -Fq 'bounded_command::run_prepared(' "$projection"
grep -Fq 'PROJECTOR_OUTPUT_LIMIT' "$projection"
grep -Fq 'PROJECTOR_TIMEOUT' "$projection"
! grep -Eq '\.output\(\)|wait_with_output\(' "$projection"

grep -Fq 'bounded_command::run_prepared(' "$capture"
grep -Fq '.process_group(0)' "$capture"
grep -Fq 'bounded_command::terminate_tree(' "$capture"

grep -Fq 'bounded_command::run_prepared(' "$catalogue"
grep -Fq 'LIST_OUTPUT_LIMIT' "$catalogue"
grep -Fq 'INSTALL_TIMEOUT' "$catalogue"
! grep -Eq '\.output\(\)|wait_with_output\(' "$catalogue"

grep -Fq 'bounded_command::run_prepared(' "$history"
grep -Fq 'NARRATIVE_TIMEOUT' "$history"
! grep -Eq '\.spawn\(\)|\.output\(\)|wait_with_output\(' "$history"

grep -Fq 'bounded_command::run_prepared(' "$firstboot"
! grep -Eq '\.output\(\)|wait_with_output\(' "$firstboot"

echo 'shell subprocess boundary: ok'
