#!/usr/bin/env bash
# Desktop OS controls share one bounded, descendant-cleaning process boundary.
# The boundary itself lives in `tilo-bounded-command`, beside the other
# platform utilities, so the OS actions, the print engine and the rest of the
# system run their helpers the same way.
set -uo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TEST_DIR/../.." && pwd)"
PROCESS="$ROOT/crates/os/os-actions/src/process.rs"
FIRMWARE="$ROOT/crates/os/os-actions/src/firmware.rs"
PRIVACY="$ROOT/crates/os/os-actions/src/privacy.rs"
OS_ACTIONS_SRC="$ROOT/crates/os/os-actions/src"
NM_CONTROL="$ROOT/crates/os/tilo-nm/src/control.rs"
RUNNER="$ROOT/crates/platform/bounded-command/src/command.rs"
BOUNDS="$ROOT/crates/platform/bounded-command/src/lib.rs"

fails=0
checks=0
fail() { echo "FAIL: $*" >&2; fails=$((fails + 1)); }
ok() { checks=$((checks + 1)); }
must_contain_in() {
    local file="$1" pattern="$2" reason="$3"
    if [ -r "$file" ] && grep -Eq "$pattern" "$file"; then ok; else fail "$reason"; fi
}
must_not_contain_in() {
    local file="$1" pattern="$2" reason="$3"
    if [ -r "$file" ] && grep -Eq "$pattern" "$file"; then fail "$reason"; else ok; fi
}
must_contain() { must_contain_in "$PROCESS" "$1" "$2"; }
must_not_contain() { must_not_contain_in "$PROCESS" "$1" "$2"; }

if [ ! -r "$PROCESS" ]; then
    echo "FAIL: cannot inspect $PROCESS" >&2
    exit 1
fi
if [ ! -r "$RUNNER" ] || [ ! -r "$BOUNDS" ]; then
    echo "FAIL: cannot inspect the shared bounded runner under $(dirname "$RUNNER")" >&2
    exit 1
fi

if [ ! -r "$PRIVACY" ]; then
    fail "cannot inspect $PRIVACY"
elif grep -Eq 'Command::new|\.spawn\(' "$PRIVACY"; then
    fail "PipeWire privacy helpers start processes outside the bounded runner"
else
    ok
fi
# The boundary is the shared runner's, not a private copy: the OS controls'
# module re-exports exactly its no-input helpers.
must_contain 'pub\(crate\) use bounded_command::command::\{run, run_capture, which\}' \
    "OS controls do not use the shared bounded process runner"
# Network secrets (Wi-Fi passwords, 802.1X passwords, WireGuard keys) travel
# to NetworkManager in D-Bus calls through tilo-nm, never through a client
# process's argv or stdin. Every OS control helper takes no input at all; the
# runner's input-taking entry points are used by nobody under os-actions.
must_contain_in "$RUNNER" 'crate::run\(binary, args, None, MAX_CAPTURE_BYTES, timeout\)' \
    "the shared runner passes an input stream to OS control helpers"
if grep -rEq 'run_prepared\(|run_with_env\(|bounded_command::run\(' "$OS_ACTIONS_SRC"; then
    fail "OS control subprocesses are fed input, which only a secret would need"
else
    ok
fi
if grep -rEq '"nmcli"' "$OS_ACTIONS_SRC"; then
    fail "OS controls hand network settings or secrets to an nmcli process"
else
    ok
fi
if [ ! -r "$NM_CONTROL" ]; then
    fail "cannot inspect $NM_CONTROL"
elif grep -q 'Zeroizing<String>' "$NM_CONTROL" && grep -q '"Unregister"' "$NM_CONTROL"; then
    ok
else
    fail "an unsaved connection secret is not zeroized or outlives its one-shot secret agent"
fi
# One place sanitizes what a helper said before it reaches a person: the
# runner's diagnostic, which the error path uses.
must_contain_in "$BOUNDS" 'pub fn stderr_diagnostic\(&self\)' \
    "OS helper diagnostics are not sanitized before display"
must_contain_in "$RUNNER" 'stderr_diagnostic\(\)' \
    "the shared runner reports a helper's raw stderr"
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
