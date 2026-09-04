#!/usr/bin/env bash
# Exercise the one intentionally capable BootPack boundary.  The Rust unit
# test verifies the manager's exact argv; this test proves the matching
# systemd execution profile does not deny the mount/capability prerequisites
# that bootc needs while staging an OSTree deployment.
set -euo pipefail

if ! systemd-run --quiet --wait --collect /usr/bin/true >/dev/null 2>&1; then
    printf '%s\n' 'skip: no usable systemd system manager for BootPack stage profile test'
    exit 0
fi

test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT HUP INT TERM
mock_bootc="$test_dir/bootc"
captured="$test_dir/argv"
mount_dir="$test_dir/mount"
mkdir "$mount_dir"

printf '%s\n' '#!/bin/sh' \
    'set -eu' \
    "printf '%s\\n' \"\$@\" > '$captured'" \
    "mount -t tmpfs tmpfs '$mount_dir'" \
    "umount '$mount_dir'" > "$mock_bootc"
chmod 0755 "$mock_bootc"

image="registry.qos.example/boot-pack@sha256:$(printf 'a%.0s' {1..64})"
systemd-run --quiet --wait --collect --pipe \
    --property=PrivateDevices=no \
    --property=NoNewPrivileges=yes \
    --property='CapabilityBoundingSet=CAP_SYS_ADMIN CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_MKNOD CAP_SETFCAP' \
    --property='SystemCallFilter=@system-service' \
    --property='SystemCallFilter=~@swap @reboot @raw-io @obsolete @cpu-emulation @module @clock @debug @pkey' \
    "$mock_bootc" switch --transport=registry "$image"

expected=$(printf 'switch\n--transport=registry\n%s\n' "$image")
[ "$(cat "$captured")" = "${expected%$'\n'}" ]
printf '%s\n' 'ok: BootPack execution profile permits fixed bootc argv and mount staging'
