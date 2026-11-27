#!/usr/bin/env bash
# Removable and malformed devices must not turn disk helpers into DoS inputs.
# Disks is a tilolang package that runs no programs: every device operation is
# an action of the device service, which runs its own helpers through the
# shared bounded runner, so no device can feed either unbounded output or
# stall it.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
app="$root/apps/tl/disks"
service="$root/crates/os/block-devices/src"

fail() {
  echo "disk subprocess boundary: $*" >&2
  exit 1
}

[ -f "$app/app.tl" ] || fail "Disks is not the tilolang package at $app"
grep -Fq 'use so.tilo.device as device' "$app/app.tl" \
  || fail "Disks does not reach its drives through the device service"
grep -Fq 'device.Drive' "$app/app.tl" \
  || fail "Disks reads no drive from the device service"
# A package ships a program only by declaring one; Disks ships none, so it has
# no process of its own to feed or stall.
[ ! -e "$app/app.toml" ] || fail "Disks ships a program of its own"

grep -rFq 'bounded_command::' "$service" \
  || fail "the device service runs no helper through the shared bounded runner"
if grep -rEn '\.spawn\(\)|wait_with_output\(' "$service"; then
  fail "the device service starts a helper outside the shared bounded runner"
fi

echo 'disk subprocess boundary: ok'
