#!/usr/bin/env bash
# Update and rollback controls must remain bounded even when system daemons wedge.
# Settings runs the update timer, firmware and rollback controls; the session's system
# service fetches, stages and applies the newest image when a person asks for an update.
set -uo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TEST_DIR/../.." && pwd)"
SYSTEM_UPDATE="$ROOT/crates/os/system-service/src/update.rs"

fails=0
checks=0
for source in "$SYSTEM_UPDATE"; do
    if [ ! -r "$source" ]; then
        echo "FAIL: cannot inspect $source" >&2
        exit 1
    fi
done
require() {
    if grep -Eq "$2" "$1"; then checks=$((checks + 1)); else
        echo "FAIL: $3 (${1#"$ROOT"/})" >&2; fails=$((fails + 1))
    fi
}
reject() {
    if grep -Eq "$2" "$1"; then
        echo "FAIL: $3 (${1#"$ROOT"/})" >&2; fails=$((fails + 1))
    else checks=$((checks + 1)); fi
}

for source in "$SYSTEM_UPDATE"; do
    require "$source" 'bounded_command::run\(' "update controls do not use bounded execution"
    reject "$source" 'Command::new|\.output\(\)|wait_with_output|\.spawn\(\)' \
        "update controls contain a raw subprocess path"
done
require "$SYSTEM_UPDATE" 'UPDATE_TIMEOUT: Duration' "network-backed image staging has no explicit deadline"
require "$SYSTEM_UPDATE" 'UPDATE_UNIT,[[:space:]]*$|UPDATE_TIMEOUT,' \
    "the image staging unit is not started under its deadline"

if [ "$fails" -gt 0 ]; then
    echo "update subprocess invariants: $fails failed, $checks passed" >&2
    exit 1
fi
echo "OK: update subprocess invariants ($checks checks)"
