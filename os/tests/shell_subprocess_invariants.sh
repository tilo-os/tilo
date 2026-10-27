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

# Each failure names its file and pattern. (A bare `! grep` would not stop the
# script under set -e: errexit ignores negated commands.)
need() { grep -Fq -- "$1" "$2" || { echo "missing in ${2#"$root"/}: $1" >&2; exit 1; }; }
forbid() { ! grep -Eq -- "$1" "$2" || { echo "forbidden in ${2#"$root"/}: $1" >&2; exit 1; }; }

# Projection talks to the projector service over its control socket and
# spawns nothing: frames are size-capped and every reply wait is bounded.
forbid 'Command::new|\.spawn\(\)|\.output\(\)|wait_with_output\(' "$projection"
need 'control::write_request(' "$projection"
need 'recv_timeout(' "$projection"
need 'CONNECT_REPLY_TIMEOUT' "$projection"
need 'DISCONNECT_REPLY_TIMEOUT' "$projection"
need 'pub const CONTROL_FRAMING: Framing = Framing::little_endian(1024 * 1024);' "$projector_control"

need 'bounded_command::run_prepared(' "$capture"
need '.process_group(0)' "$capture"
need 'bounded_command::terminate_tree(' "$capture"

need 'bounded_command::run_prepared(' "$catalogue"
need 'LIST_OUTPUT_LIMIT' "$catalogue"
need 'INSTALL_TIMEOUT' "$catalogue"
forbid '\.output\(\)|wait_with_output\(' "$catalogue"

# History prose asks the on-device model provider over HTTP and spawns
# nothing: the request is time-bounded and the answer size-capped.
forbid 'Command::new|\.spawn\(\)|\.output\(\)|wait_with_output\(' "$history"
need '.timeout(NARRATIVE_TIMEOUT)' "$history"
need '.bytes_limited(NARRATIVE_RESPONSE_LIMIT)' "$history"
forbid '\.bytes\(\)' "$history"

need 'bounded_command::run_prepared(' "$firstboot"
forbid '\.output\(\)|wait_with_output\(' "$firstboot"

echo 'shell subprocess boundary: ok'
