#!/usr/bin/env bash
# Retained as a fail-closed tombstone for callers outside this repository.
# The historical fixture retained /usr/libexec/tilo-session-runner and could
# stop the caller's real graphical-session targets. Real handoff coverage
# belongs in a booted disposable VM with no route to the developer's manager.
set -euo pipefail

echo 'DISABLED: this historical fixture is not an isolation boundary; use a disposable VM handoff test' >&2
exit 64
