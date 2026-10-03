#!/usr/bin/env bash
# Desktop OS controls share one bounded, descendant-cleaning process boundary.
set -uo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TEST_DIR/../.." && pwd)"
PROCESS="$ROOT/crates/system/os-actions/src/process.rs"
FIRMWARE="$ROOT/crates/system/os-actions/src/firmware.rs"
PRIVACY="$ROOT/crates/system/os-actions/src/privacy.rs"

fails=0
checks=0
fail() { echo "FAIL: $*" >&2; fails=$((fails + 1)); }
ok() { checks=$((checks + 1)); }
must_contain() {
    local pattern="$1" reason="$2"
    if grep -Eq "$pattern" "$PROCESS"; then ok; else fail "$reason"; fi
}
must_not_contain() {
    local pattern="$1" reason="$2"
    if grep -Eq "$pattern" "$PROCESS"; then fail "$reason"; else ok; fi
}

if [ ! -r "$PROCESS" ]; then
    echo "FAIL: cannot inspect $PROCESS" >&2
    exit 1
fi

if [ ! -r "$PRIVACY" ]; then
    fail "cannot inspect $PRIVACY"
else
    if grep -Eq '\[0_u8; 8192\]' "$PRIVACY"; then ok; else
        fail "PipeWire monitor reads daemon output with unbounded line allocation"
    fi
    if grep -Eq '\.process_group\(0\)' "$PRIVACY"; then ok; else
        fail "PipeWire monitor lacks isolated process ownership"
    fi
    if grep -Eq 'terminate_monitor' "$PRIVACY"; then ok; else
        fail "PipeWire monitor errors can leave descendants alive"
    fi
fi
must_contain 'bounded_command::run\(' \
    "OS controls do not use the shared bounded process runner"
must_contain 'Some\(input\)' \
    "secret-bearing OS controls do not pass input through the bounded runner"
must_contain 'sanitized_diagnostic' \
    "OS helper diagnostics are not sanitized before display"
must_not_contain '\.spawn\(\)|\.output\(\)|wait_with_output' \
    "the shared OS action boundary contains raw subprocess execution"
must_not_contain 'child\.kill\(\)' \
    "direct-child-only cleanup can leave helper descendants alive"

if [ ! -r "$FIRMWARE" ]; then
    fail "cannot inspect $FIRMWARE"
else
    if grep -Eq '\.process_group\(0\)' "$FIRMWARE"; then ok; else
        fail "streaming firmware updates lack isolated process ownership"
    fi
    if grep -Eq 'MAX_FIRMWARE_(STREAM|LINE)_BYTES|MAX_FIRMWARE_EVENTS' "$FIRMWARE"; then ok; else
        fail "firmware progress output is not allocation/event bounded"
    fi
    if grep -Eq 'bounded_command::terminate_tree' "$FIRMWARE"; then ok; else
        fail "firmware timeout/error cleanup can leave descendants alive"
    fi
fi

if [ "$fails" -gt 0 ]; then
    echo "OS action subprocess invariants: $fails failed, $checks passed" >&2
    exit 1
fi
echo "OK: OS action subprocess invariants ($checks checks)"
