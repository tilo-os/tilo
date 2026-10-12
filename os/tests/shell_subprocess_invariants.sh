#!/usr/bin/env bash
# Shell-owned helper boundaries must cap allocation, duration, and descendants.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
projection="$root/crates/session/shell/src/shell/projection.rs"
projector_control="$root/crates/system/projector/src/control.rs"
capture="$root/crates/session/shell/src/compositor/audio_capture.rs"
catalogue="$root/crates/session/shell/src/shell/catalogue.rs"
history="$root/crates/session/shell/src/shell/history_narrative.rs"
firstboot="$root/crates/session/shell/src/firstboot.rs"

# Projection talks to the projector service over its control socket and
# spawns nothing: frames are size-capped and every reply wait is bounded.
! grep -Eq 'Command::new|\.spawn\(\)|\.output\(\)|wait_with_output\(' "$projection"
grep -Fq 'control::write_request(' "$projection"
grep -Fq 'recv_timeout(' "$projection"
grep -Fq 'CONNECT_REPLY_TIMEOUT' "$projection"
grep -Fq 'DISCONNECT_REPLY_TIMEOUT' "$projection"
grep -Fq 'pub const CONTROL_FRAMING: Framing = Framing::little_endian(1024 * 1024);' "$projector_control"

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
