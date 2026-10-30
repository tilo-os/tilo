#!/usr/bin/env bash
# Run what CI / affected packages runs for this branch, locally, with the same
# package selection, features, filters and environment, so a push does not find
# a failure CI would have.
#
#   bash tools/ci/local-affected.sh [--base REF] [--stage STAGES] [--tapps]
#                                   [--fail-fast] [--split auto|always|never]
#
# Run it directly, not through tdev (which bypasses the local memory cap).
#
#   --base REF     comparison base (default: merge-base with origin/main; run
#                  `git fetch origin` first, and rebase when main moved)
#   --stage LIST   comma list of: check, clippy, test, goldens, certify,
#                  scenarios, scoped, tapps (default: all but tapps)
#   --tapps        also package the size-budgeted tapps (a release build:
#                  slow) and check them against their budgets
#   --fail-fast    stop the other jobs at the first failing one (the default
#                  runs every job to its end and summarises)
#   --split MODE   share a big test set between the x86 and arm builders
#
# Selection mirrors .forgejo/workflows/check.yml (tools/ci/affected_packages.py
# with the shared filters); how it runs (lint and tests at once, each on the
# builder that compiles it, failed tests first) is tools/ci/local_affected.py's.
# tools/ci/test-changed.sh is the same run for a change still being edited.
exec python3 "$(dirname "$0")/local_affected.py" "$@"
