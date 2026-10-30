#!/usr/bin/env bash
# Test what you changed: the checks and tests of the packages your change affects,
# with your uncommitted edits, on the builders, concurrently.
#
#   tools/ci/test-changed.sh                     # clippy + tests of what the change affects
#   tools/ci/test-changed.sh -- overview::       # only tests whose names match
#   tools/ci/test-changed.sh -E 'package(shell)' # only tests in this nextest filterset
#   tools/ci/test-changed.sh --stage test        # skip clippy
#   tools/ci/test-changed.sh --base origin/main  # against another base than `integration`
#   tools/ci/test-changed.sh --closure           # lint every reverse dependency, as the batch does
#
# Tests that failed last time against the same base run first. Run it directly (not
# through tdev). tools/ci/local_affected.py describes what runs where;
# docs/development/testing.md when to use what.
exec python3 "$(dirname "$0")/local_affected.py" --changed "$@"
