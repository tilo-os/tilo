#!/usr/bin/env bash
# Build the binaries of other packages that a package's tests start, before
# the tests run: a nextest setup script (.config/nextest.toml).
#
#   tools/build-sibling-binaries.sh PACKAGE:BIN...
#
# A test finds such a binary through tilo_test_env::sibling_binary, which asks
# Cargo for it the first time a process of the run needs it. Built there, the
# compile counts against that test's hang bound, and after a change the apps
# depend on it takes longer than the bound. Built here, before any test starts.
#
# The binaries are built in one invocation, which unifies their features
# differently from building each package alone; a test asking Cargo for one
# of them again would rebuild it. So each binary's path is handed to the
# run's tests as TILO_SIBLING_BIN_<bin, - as _> (through nextest's NEXTEST_ENV), which
# sibling_binary answers with before asking Cargo.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "usage: $0 PACKAGE:BIN..." >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
arguments=(build --locked --message-format=json-render-diagnostics)
bins=()
for binary; do
    arguments+=(--package "${binary%%:*}" --bin "${binary#*:}")
    bins+=("${binary#*:}")
done
cd "$repo"
messages=$("${CARGO:-cargo}" "${arguments[@]}")
for bin in "${bins[@]}"; do
    # The artifact message of each binary names the executable it wrote.
    path=$(grep -o "\"executable\":\"[^\"]*/${bin}\"" <<<"$messages" | tail -n 1 |
        sed -e 's/^"executable":"//' -e 's/"$//')
    if [[ -z "$path" || ! -x "$path" ]]; then
        echo "$0: cargo named no executable for $bin" >&2
        exit 1
    fi
    if [[ -n "${NEXTEST_ENV:-}" ]]; then
        echo "TILO_SIBLING_BIN_${bin//-/_}=${path}" >>"$NEXTEST_ENV"
    fi
done
