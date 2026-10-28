#!/usr/bin/env bash
# Cargo runner for the real editor benchmark; rcargo bench executes it remotely.
# Use --fetch iai --fetch iai-built before bench and this Cargo option after it:
# --config 'target.x86_64-unknown-linux-gnu.runner="tools/perf/typing-rcargo-runner.sh"'
# Keep the pinned build image and disable rcargo's local fallback.
set -euo pipefail
# Cargo runs a benchmark from its package directory, not the workspace root.
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
bash tools/perf/build-typing-tools.sh
exec bash tools/perf/typing-iai.sh --run-executable "$@"
