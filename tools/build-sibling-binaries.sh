#!/usr/bin/env bash
# Build the binaries of other packages that a package's tests start, before
# the tests run: a nextest setup script (.config/nextest.toml).
#
#   tools/build-sibling-binaries.sh PACKAGE:BIN...
#
# A test finds such a binary through tilo_test_env::sibling_binary, which asks
# Cargo for it the first time a process of the run needs it. Built there, the
# compile counts against that test's hang bound, and after a change the apps
# depend on it takes longer than the bound. Built here, before any test starts,
# the test's own request finds it up to date.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "usage: $0 PACKAGE:BIN..." >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
arguments=(build --locked)
for binary; do
    arguments+=(--package "${binary%%:*}" --bin "${binary#*:}")
done
cd "$repo"
exec "${CARGO:-cargo}" "${arguments[@]}"
