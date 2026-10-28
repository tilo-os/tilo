#!/usr/bin/env bash
# Cargo runner for the real editor benchmark, using Cargo's built executable.
# Pass this option to cargo bench (or any cargo drop-in):
# --config 'target.x86_64-unknown-linux-gnu.runner="tools/perf/typing-cargo-runner.sh"'
# Preserve the compiler/build inputs and retain target/iai and target/iai-built.
set -euo pipefail
# Cargo runs a benchmark from its package directory, not the workspace root.
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
bash tools/perf/build-typing-tools.sh
exec bash tools/perf/typing-iai.sh --run-executable "$@"
