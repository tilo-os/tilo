#!/usr/bin/env bash
# Update and rollback controls must remain bounded even when system daemons wedge.
set -uo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TEST_DIR/../.." && pwd)"
SOURCE="$ROOT/apps/settings-rare/src/panes/updates.rs"

if [ ! -r "$SOURCE" ]; then
    echo "FAIL: cannot inspect $SOURCE" >&2
    exit 1
fi
fails=0
checks=0
require() {
    if grep -Eq "$1" "$SOURCE"; then checks=$((checks + 1)); else
        echo "FAIL: $2" >&2; fails=$((fails + 1))
    fi
}
reject() {
    if grep -Eq "$1" "$SOURCE"; then
        echo "FAIL: $2" >&2; fails=$((fails + 1))
    else checks=$((checks + 1)); fi
}

require 'bounded_command::run\(' "update controls do not use bounded execution"
require 'LONG_UPDATE_TIMEOUT' "network-backed image staging has no explicit deadline"
require 'stderr_diagnostic\(\)' "update helper diagnostics are not sanitized"
reject 'Command::new|\.output\(\)|wait_with_output|\.spawn\(\)' \
    "update controls contain a raw subprocess path"

if [ "$fails" -gt 0 ]; then
    echo "update subprocess invariants: $fails failed, $checks passed" >&2
    exit 1
fi
echo "OK: update subprocess invariants ($checks checks)"
