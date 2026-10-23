#!/usr/bin/env bash
# Removable and malformed devices must not turn disk helpers into DoS inputs:
# Disks runs no programs at all. Every device operation is a udisks2 call
# behind polkit, so no device can feed it unbounded output or stall it.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
src="$root/apps/disks/src"

grep -Fq 'udisks' "$src/ops.rs"
! grep -rEn 'std::process|Command::new|\.output\(\)|wait_with_output\(|\.spawn\(\)' "$src"

echo 'disk subprocess boundary: ok'
